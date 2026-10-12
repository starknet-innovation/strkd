//! Note, channel and subchannel encryption, mirroring the pool's `utils.cairo`.
//!
//! The pool encrypts by field addition (`enc = H(..) + plaintext` mod p) and note
//! amounts by addition mod 2^128. Channel info is sent to the recipient with ECDH
//! on the Stark curve: an ephemeral point's x-coordinate is published, and both
//! sides derive the shared x-coordinate from it.
//!
//! Only decryption is needed by a wallet: the pool's `compile_actions` performs
//! every encryption inside the proof.

use starknet_types_core::curve::AffinePoint;
use starknet_types_core::felt::Felt;

use crate::hashes;
use crate::Error;

/// The x-coordinate of `sk·G`, the pool's notion of a public key.
pub fn public_key(sk: Felt) -> Felt {
    (&AffinePoint::generator() * sk).x()
}

/// ECDH: the x-coordinate of `sk·P`, where `P` is either point with x-coordinate
/// `public_x`. Both points give the same x, so the y-parity is irrelevant.
pub fn shared_x(sk: Felt, public_x: Felt) -> Result<Felt, Error> {
    let point = AffinePoint::new_from_x(&public_x, false).ok_or(Error::NotOnCurve(public_x))?;
    Ok((&point * sk).x())
}

/// An incoming channel, as stored by the pool for its recipient
/// (`get_channel_info`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncChannelInfo {
    pub ephemeral_pubkey: Felt,
    pub enc_channel_key: Felt,
    pub enc_sender_addr: Felt,
}

/// A decrypted incoming channel: its key and who opened it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelInfo {
    pub key: Felt,
    pub sender: Felt,
}

pub fn decrypt_channel_info(
    encrypted: &EncChannelInfo,
    recipient_private_key: Felt,
) -> Result<ChannelInfo, Error> {
    let shared = shared_x(recipient_private_key, encrypted.ephemeral_pubkey)?;
    Ok(ChannelInfo {
        key: encrypted.enc_channel_key - hashes::enc_channel_key_hash(shared),
        sender: encrypted.enc_sender_addr - hashes::enc_sender_addr_hash(shared),
    })
}

/// A subchannel (one per token within a channel). `salt == 0` means "no
/// subchannel at this index" — the end of a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncSubchannelInfo {
    pub salt: Felt,
    pub enc_token: Felt,
}

/// The token a subchannel carries.
pub fn decrypt_subchannel_token(
    encrypted: &EncSubchannelInfo,
    channel_key: Felt,
    index: u32,
) -> Felt {
    encrypted.enc_token - hashes::enc_token_hash(channel_key, index, encrypted.salt)
}

/// One of the sender's own outgoing channels. `salt == 0` ends a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncOutgoingChannelInfo {
    pub salt: Felt,
    pub enc_recipient_addr: Felt,
}

pub fn decrypt_outgoing_recipient(
    encrypted: &EncOutgoingChannelInfo,
    sender_addr: Felt,
    sender_private_key: Felt,
    index: u32,
) -> Felt {
    encrypted.enc_recipient_addr
        - hashes::enc_recipient_addr_hash(sender_addr, sender_private_key, index, encrypted.salt)
}

/// Salt marking an open note: its amount is stored in the clear.
pub const OPEN_NOTE_SALT: u128 = 1;

/// A note's stored value, `packed_value = (salt << 128) | enc_amount`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteValue {
    pub amount: u128,
    pub salt: u128,
    pub open: bool,
}

fn two_pow_128() -> Felt {
    Felt::from(1u128 << 127) * Felt::TWO
}

fn split_u128s(value: Felt) -> (u128, u128) {
    let bytes = value.to_bytes_be();
    let hi = u128::from_be_bytes(bytes[..16].try_into().unwrap());
    let lo = u128::from_be_bytes(bytes[16..].try_into().unwrap());
    (hi, lo)
}

/// Decrypt a note's packed value. `None` for an empty slot (`packed_value == 0`).
pub fn decrypt_note_value(
    packed_value: Felt,
    channel_key: Felt,
    token: Felt,
    index: u32,
) -> Option<NoteValue> {
    if packed_value == Felt::ZERO {
        return None;
    }
    let (salt, enc_amount) = split_u128s(packed_value);
    if salt == OPEN_NOTE_SALT {
        return Some(NoteValue {
            amount: enc_amount,
            salt,
            open: true,
        });
    }
    // The pad is the hash reduced mod 2^128, i.e. its low 128 bits.
    let (_, pad) = split_u128s(hashes::enc_amount_hash(
        channel_key,
        token,
        index,
        Felt::from(salt),
    ));
    Some(NoteValue {
        amount: enc_amount.wrapping_sub(pad),
        salt,
        open: false,
    })
}

/// Inverse of [`decrypt_note_value`] for an encrypted note (salt ≥ 2). Used by
/// tests and by callers that predict the pool's writes.
pub fn encrypt_note_value(
    amount: u128,
    salt: u128,
    channel_key: Felt,
    token: Felt,
    index: u32,
) -> Felt {
    let (_, pad) = split_u128s(hashes::enc_amount_hash(
        channel_key,
        token,
        index,
        Felt::from(salt),
    ));
    let enc_amount = pad.wrapping_add(amount);
    Felt::from(salt) * two_pow_128() + Felt::from(enc_amount)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecdh_agrees_both_ways() {
        let (a, b) = (Felt::from(0x1234_5678u64), Felt::from(0x9abc_def0u64));
        assert_eq!(
            shared_x(a, public_key(b)).unwrap(),
            shared_x(b, public_key(a)).unwrap()
        );
    }

    #[test]
    fn note_value_round_trips() {
        let (ck, token) = (Felt::from(77u64), Felt::from(0x4718u64));
        for amount in [0u128, 1, 10u128.pow(18), u128::MAX] {
            let packed = encrypt_note_value(amount, 0xabcdef, ck, token, 3);
            let v = decrypt_note_value(packed, ck, token, 3).unwrap();
            assert_eq!((v.amount, v.salt, v.open), (amount, 0xabcdef, false));
        }
    }

    #[test]
    fn open_note_amount_is_plaintext() {
        let packed = Felt::from(OPEN_NOTE_SALT) * two_pow_128() + Felt::from(42u64);
        let v = decrypt_note_value(packed, Felt::ONE, Felt::ONE, 0).unwrap();
        assert_eq!((v.amount, v.open), (42, true));
    }

    #[test]
    fn empty_note_slot() {
        assert_eq!(
            decrypt_note_value(Felt::ZERO, Felt::ONE, Felt::ONE, 0),
            None
        );
    }
}
