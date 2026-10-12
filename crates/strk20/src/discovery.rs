//! Discovery: reading a user's private state from the pool's storage.
//!
//! A port of the SDK's on-chain `ContractDiscoveryProvider`. Everything is read
//! through the pool's view functions and decrypted locally, so no indexer ever
//! sees `user_sk`. Reads are sequential scans: each list (incoming channels,
//! subchannels, notes, outgoing channels) ends at the first empty slot.
//!
//! Implementors provide [`PoolReader::call`], a `starknet_call` on the pool
//! pinned to one block. Pin it to the proving block when planning a proof, so
//! discovery and the proof see the same state.

use std::collections::BTreeMap;

use async_trait::async_trait;
use starknet_types_core::felt::Felt;
use wallet_core::get_selector_from_name;

use crate::crypto::{self, EncChannelInfo, EncOutgoingChannelInfo, EncSubchannelInfo};
use crate::{hashes, Error};

/// Read access to one pool at one block.
#[async_trait]
pub trait PoolReader: Send + Sync {
    /// Call a view function of the pool; returns its raw output felts.
    async fn call(&self, selector: Felt, calldata: Vec<Felt>) -> Result<Vec<Felt>, Error>;
}

async fn view<R: PoolReader + ?Sized>(
    r: &R,
    name: &str,
    calldata: Vec<Felt>,
    n: usize,
) -> Result<Vec<Felt>, Error> {
    let out = r.call(get_selector_from_name(name), calldata).await?;
    if out.len() < n {
        return Err(Error::Read(format!(
            "{name} returned {} felts, expected {n}",
            out.len()
        )));
    }
    Ok(out)
}

/// The viewing public key `user` registered, or `None`.
pub async fn public_key<R: PoolReader + ?Sized>(r: &R, user: Felt) -> Result<Option<Felt>, Error> {
    let pk = view(r, "get_public_key", vec![user], 1).await?[0];
    Ok((pk != Felt::ZERO).then_some(pk))
}

async fn flag<R: PoolReader + ?Sized>(r: &R, name: &str, arg: Felt) -> Result<bool, Error> {
    Ok(view(r, name, vec![arg], 1).await?[0] != Felt::ZERO)
}

async fn note_packed_value<R: PoolReader + ?Sized>(r: &R, note_id: Felt) -> Result<Felt, Error> {
    // get_note -> Note { packed_value, token }
    Ok(view(r, "get_note", vec![note_id], 1).await?[0])
}

async fn subchannel_info<R: PoolReader + ?Sized>(
    r: &R,
    id: Felt,
) -> Result<EncSubchannelInfo, Error> {
    let out = view(r, "get_subchannel_info", vec![id], 2).await?;
    Ok(EncSubchannelInfo {
        salt: out[0],
        enc_token: out[1],
    })
}

fn index_u32(i: usize) -> Result<u32, Error> {
    u32::try_from(i).map_err(|_| Error::Read("index overflow".into()))
}

/// A note the user can spend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub id: Felt,
    pub token: Felt,
    pub amount: u128,
    /// Key of the channel the note was sent on (its spend witness).
    pub channel_key: Felt,
    /// The note's index within its subchannel.
    pub index: u32,
    /// Who opened the channel (the user, for change and deposits).
    pub sender: Felt,
    pub open: bool,
}

/// Every unspent note addressed to `user`, across all incoming channels.
/// `tokens` filters by token; empty means all.
pub async fn discover_notes<R: PoolReader + ?Sized>(
    r: &R,
    user: Felt,
    user_sk: Felt,
    tokens: &[Felt],
) -> Result<Vec<Note>, Error> {
    let count = view(r, "get_num_of_channels", vec![user], 1).await?[0];
    let count = u64::try_from(count).map_err(|_| Error::Read("channel count overflow".into()))?;
    let mut notes = Vec::new();
    for c in 0..count {
        let out = view(r, "get_channel_info", vec![user, Felt::from(c)], 3).await?;
        let enc = EncChannelInfo {
            ephemeral_pubkey: out[0],
            enc_channel_key: out[1],
            enc_sender_addr: out[2],
        };
        let channel = crypto::decrypt_channel_info(&enc, user_sk)?;
        for k in 0.. {
            let k = index_u32(k)?;
            let sub = subchannel_info(r, hashes::subchannel_id(channel.key, k)).await?;
            if sub.salt == Felt::ZERO {
                break;
            }
            let token = crypto::decrypt_subchannel_token(&sub, channel.key, k);
            if !tokens.is_empty() && !tokens.contains(&token) {
                continue;
            }
            for i in 0.. {
                let i = index_u32(i)?;
                let id = hashes::note_id(channel.key, token, i);
                let Some(value) = crypto::decrypt_note_value(
                    note_packed_value(r, id).await?,
                    channel.key,
                    token,
                    i,
                ) else {
                    break;
                };
                let nullifier = hashes::nullifier(channel.key, token, i, user_sk);
                if flag(r, "nullifier_exists", nullifier).await? {
                    continue;
                }
                notes.push(Note {
                    id,
                    token,
                    amount: value.amount,
                    channel_key: channel.key,
                    index: i,
                    sender: channel.sender,
                    open: value.open,
                });
            }
        }
    }
    Ok(notes)
}

/// A token subchannel inside one of the user's outgoing channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subchannel {
    /// Its index within the channel.
    pub index: u32,
    /// The index the next note created on it takes.
    pub next_note_index: u32,
}

/// The user's view of a channel it sends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingChannel {
    pub recipient: Felt,
    pub recipient_public_key: Felt,
    /// Deterministic: `channel_key(user, user_sk, recipient, recipient_pk)`.
    pub key: Felt,
    /// Whether the pool already holds it (else it must be opened first).
    pub open: bool,
    pub subchannels: BTreeMap<Felt, Subchannel>,
}

/// The user's channel to `recipient`, opened or not. `Err(NotRegistered)` if the
/// recipient has no viewing key (nothing can be sent to them).
pub async fn discover_channel<R: PoolReader + ?Sized>(
    r: &R,
    user: Felt,
    user_sk: Felt,
    recipient: Felt,
) -> Result<OutgoingChannel, Error> {
    let recipient_public_key = public_key(r, recipient)
        .await?
        .ok_or(Error::NotRegistered(recipient))?;
    let key = hashes::channel_key(user, user_sk, recipient, recipient_public_key);
    let marker = hashes::channel_marker(key, user, recipient, recipient_public_key);
    let open = flag(r, "channel_exists", marker).await?;
    let mut subchannels = BTreeMap::new();
    if open {
        for k in 0.. {
            let k = index_u32(k)?;
            let sub = subchannel_info(r, hashes::subchannel_id(key, k)).await?;
            if sub.salt == Felt::ZERO {
                break;
            }
            let token = crypto::decrypt_subchannel_token(&sub, key, k);
            let mut next = 0u32;
            while note_packed_value(r, hashes::note_id(key, token, next)).await? != Felt::ZERO {
                next += 1;
            }
            subchannels.insert(
                token,
                Subchannel {
                    index: k,
                    next_note_index: next,
                },
            );
        }
    }
    Ok(OutgoingChannel {
        recipient,
        recipient_public_key,
        key,
        open,
        subchannels,
    })
}

/// How many channels the user has opened: the index its next channel takes.
pub async fn outgoing_channel_count<R: PoolReader + ?Sized>(
    r: &R,
    user: Felt,
    user_sk: Felt,
) -> Result<u32, Error> {
    for s in 0.. {
        let s = index_u32(s)?;
        let out = view(
            r,
            "get_outgoing_channel_info",
            vec![hashes::outgoing_channel_id(user, user_sk, s)],
            2,
        )
        .await?;
        let info = EncOutgoingChannelInfo {
            salt: out[0],
            enc_recipient_addr: out[1],
        };
        if info.salt == Felt::ZERO {
            return Ok(s);
        }
    }
    unreachable!("the loop returns at the first empty slot or on index overflow")
}
