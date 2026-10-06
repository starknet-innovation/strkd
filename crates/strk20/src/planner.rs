//! Planning: turning a user's intent into an ordered list of client actions.
//!
//! A port of the SDK's `ActionCompiler` for the operations strkd supports:
//! register, deposit, private transfer and withdraw, in any combination within
//! one batch. It follows the SDK's rules:
//! - per token, `deposits + spent notes = transfers + withdrawals + change`;
//!   a deficit is covered by spending notes, largest first; a surplus becomes a
//!   change note to the user;
//! - a missing channel is opened at index = the user's outgoing-channel count,
//!   a missing subchannel at index = the channel's subchannel count, and each
//!   new note takes its subchannel's next note index;
//! - registering also opens the user's channel to itself (bramble's
//!   "activate private balances");
//! - actions are emitted in the pool's phase order.
//!
//! The planner is pure: the caller supplies discovered state ([`State`]) and a
//! randomness source, which keeps it testable against the SDK with fixed
//! randomness.

use std::collections::BTreeMap;

use starknet_types_core::felt::Felt;

use crate::actions::ClientAction;
use crate::crypto;
use crate::discovery::{Note, OutgoingChannel};
use crate::{hashes, Error};

/// Randomness for salts and encryption nonces.
pub trait Randomness {
    /// A uniformly random felt below the curve order (the SDK's `generateRandom`).
    fn felt(&mut self) -> Result<Felt, Error>;
    /// A random 120-bit note salt (the SDK's `generateRandom120`).
    fn salt120(&mut self) -> Result<u128, Error>;
}

/// The OS RNG.
pub struct OsRandomness;

impl Randomness for OsRandomness {
    fn felt(&mut self) -> Result<Felt, Error> {
        // 31 random bytes = 248 bits, always below the curve order (~2^251.9).
        let mut b = [0u8; 31];
        getrandom::getrandom(&mut b).map_err(|e| Error::Random(e.to_string()))?;
        Ok(Felt::from_bytes_be_slice(&b))
    }

    fn salt120(&mut self) -> Result<u128, Error> {
        let mut b = [0u8; 16];
        getrandom::getrandom(&mut b[1..]).map_err(|e| Error::Random(e.to_string()))?;
        Ok(u128::from_be_bytes(b))
    }
}

/// What the user wants this batch to do.
#[derive(Debug, Clone, Default)]
pub struct Intent {
    /// Register the viewing key (and open the self-channel) first.
    pub register: bool,
    /// Move public funds into the pool: `(token, amount)`.
    pub deposits: Vec<(Felt, u128)>,
    /// Private transfers: `(recipient, token, amount)`.
    pub transfers: Vec<(Felt, Felt, u128)>,
    /// Withdrawals to public addresses: `(to, token, amount)`. A relayer's fee is
    /// one of these.
    pub withdrawals: Vec<(Felt, Felt, u128)>,
}

/// Discovered state the plan builds on.
#[derive(Debug, Clone)]
pub struct State {
    pub user: Felt,
    pub user_sk: Felt,
    /// Whether the user's viewing key is already registered.
    pub registered: bool,
    /// The user's outgoing-channel count.
    pub outgoing_channels: u32,
    /// The user's channels to every recipient this batch sends to, the user
    /// itself included (its own change and deposit notes travel on it).
    pub channels: BTreeMap<Felt, OutgoingChannel>,
    /// The user's unspent notes.
    pub notes: Vec<Note>,
}

/// A planned batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub actions: Vec<ClientAction>,
    /// Notes this batch spends.
    pub spent: Vec<Felt>,
    /// Change returned to the user, per token.
    pub change: BTreeMap<Felt, u128>,
}

/// Plan a batch. `state.channels` must hold every transfer recipient and the
/// user (see [`State::channels`]).
pub fn plan(intent: &Intent, state: &State, rng: &mut dyn Randomness) -> Result<Plan, Error> {
    let user = state.user;
    if !intent.register && !state.registered {
        return Err(Error::NotRegistered(user));
    }
    for &(_, amount) in &intent.deposits {
        nonzero(amount)?;
    }
    for &(_, _, amount) in intent.transfers.iter().chain(&intent.withdrawals) {
        nonzero(amount)?;
    }

    // Per-token balance: + deposits - transfers - withdrawals.
    let mut balance: BTreeMap<Felt, i128> = BTreeMap::new();
    let signed = |a: u128| i128::try_from(a).map_err(|_| Error::Read("amount exceeds i128".into()));
    for &(token, amount) in &intent.deposits {
        *balance.entry(token).or_default() += signed(amount)?;
    }
    for &(_, token, amount) in intent.transfers.iter().chain(&intent.withdrawals) {
        *balance.entry(token).or_default() -= signed(amount)?;
    }

    // Cover deficits with notes, largest first (resists dust-note griefing).
    let mut spends = Vec::new();
    let mut change = BTreeMap::new();
    for (&token, bal) in balance.iter_mut() {
        if *bal < 0 {
            let mut candidates: Vec<&Note> = state
                .notes
                .iter()
                .filter(|n| n.token == token && !n.open && n.amount > 0)
                .collect();
            candidates.sort_by_key(|n| std::cmp::Reverse(n.amount));
            for note in candidates {
                if *bal >= 0 {
                    break;
                }
                *bal += signed(note.amount)?;
                spends.push(note);
            }
            if *bal < 0 {
                let available = state
                    .notes
                    .iter()
                    .filter(|n| n.token == token)
                    .map(|n| n.amount)
                    .sum();
                return Err(Error::InsufficientBalance {
                    token,
                    shortfall: bal.unsigned_abs(),
                    available,
                });
            }
        }
        if *bal > 0 {
            change.insert(token, *bal as u128);
        }
    }

    // New notes: transfers first, then change to the user (SDK order).
    let mut new_notes: Vec<(Felt, Felt, u128)> = intent.transfers.clone();
    new_notes.extend(change.iter().map(|(&token, &amount)| (user, token, amount)));

    let mut channels = state.channels.clone();
    let mut next_channel_index = state.outgoing_channels;
    let mut account = Vec::new();
    let mut open_channels = Vec::new();
    let mut open_subchannels = Vec::new();
    let mut create_notes = Vec::new();

    if intent.register && !state.registered {
        account.push(ClientAction::SetViewingKey {
            random: rng.felt()?,
        });
        // The self-channel can't exist before registration; open it now.
        let pk = crypto::public_key(state.user_sk);
        let key = hashes::channel_key(user, state.user_sk, user, pk);
        channels.insert(
            user,
            OutgoingChannel {
                recipient: user,
                recipient_public_key: pk,
                key,
                open: false,
                subchannels: BTreeMap::new(),
            },
        );
    }

    let mut ensure_channel = |recipient: Felt,
                              channels: &mut BTreeMap<Felt, OutgoingChannel>,
                              rng: &mut dyn Randomness|
     -> Result<(), Error> {
        let channel = channels
            .get_mut(&recipient)
            .ok_or(Error::NotRegistered(recipient))?;
        if !channel.open {
            open_channels.push(ClientAction::OpenChannel {
                recipient_addr: recipient,
                index: next_channel_index,
                random: rng.felt()?,
                salt: rng.felt()?,
            });
            next_channel_index += 1;
            channel.open = true;
        }
        Ok(())
    };

    if intent.register && !state.registered {
        ensure_channel(user, &mut channels, rng)?;
    }
    for &(recipient, token, amount) in &new_notes {
        ensure_channel(recipient, &mut channels, rng)?;
        let channel = channels.get_mut(&recipient).expect("ensured above");
        let next_sub = channel.subchannels.len() as u32;
        let sub = match channel.subchannels.get(&token) {
            Some(sub) => *sub,
            None => {
                open_subchannels.push(ClientAction::OpenSubchannel {
                    recipient_addr: recipient,
                    recipient_public_key: channel.recipient_public_key,
                    channel_key: channel.key,
                    index: next_sub,
                    token,
                    salt: rng.felt()?,
                });
                let sub = crate::discovery::Subchannel {
                    index: next_sub,
                    next_note_index: 0,
                };
                channel.subchannels.insert(token, sub);
                sub
            }
        };
        create_notes.push(ClientAction::CreateEncNote {
            recipient_addr: recipient,
            recipient_public_key: channel.recipient_public_key,
            token,
            amount,
            index: sub.next_note_index,
            salt: note_salt(rng)?,
        });
        channel
            .subchannels
            .get_mut(&token)
            .expect("inserted above")
            .next_note_index += 1;
    }

    let mut actions = account;
    actions.extend(open_channels);
    actions.extend(open_subchannels);
    actions.extend(
        intent
            .deposits
            .iter()
            .map(|&(token, amount)| ClientAction::Deposit { token, amount }),
    );
    actions.extend(spends.iter().map(|n| ClientAction::UseNote {
        channel_key: n.channel_key,
        token: n.token,
        index: n.index,
    }));
    actions.extend(create_notes);
    for &(to_addr, token, amount) in &intent.withdrawals {
        actions.push(ClientAction::Withdraw {
            to_addr,
            token,
            amount,
            random: rng.felt()?,
        });
    }
    debug_assert!(actions.windows(2).all(|w| w[0].phase() <= w[1].phase()));

    Ok(Plan {
        actions,
        spent: spends.iter().map(|n| n.id).collect(),
        change,
    })
}

fn nonzero(amount: u128) -> Result<(), Error> {
    if amount == 0 {
        Err(Error::ZeroAmount)
    } else {
        Ok(())
    }
}

/// A note salt: 120 bits, never 0 or 1 (1 marks an open note).
fn note_salt(rng: &mut dyn Randomness) -> Result<u128, Error> {
    loop {
        let salt = rng.salt120()?;
        if salt > crypto::OPEN_NOTE_SALT {
            return Ok(salt);
        }
    }
}
