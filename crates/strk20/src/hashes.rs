//! The pool's domain-separated hashes, mirroring `packages/privacy/src/hashes.cairo`.
//!
//! Every hash is `poseidon_hash_many([tag, ...inputs])`, where `tag` is a Cairo
//! short string such as `'NOTE_ID_TAG:V1'`. Every index is followed by a `0` felt:
//! the Cairo source reserves that slot "for forward compatibility, occupying the
//! position of a future hash component".

use starknet_types_core::felt::Felt;
use starknet_types_core::hash::{Poseidon, StarkHash};

const CHANNEL_MARKER_TAG: &str = "CHANNEL_MARKER_TAG:V1";
const CHANNEL_KEY_TAG: &str = "CHANNEL_KEY_TAG:V1";
const SUBCHANNEL_MARKER_TAG: &str = "SUBCHANNEL_MARKER_TAG:V1";
const SUBCHANNEL_ID_TAG: &str = "SUBCHANNEL_ID_TAG:V1";
const NULLIFIER_TAG: &str = "NULLIFIER_TAG:V1";
const ENC_CHANNEL_KEY_TAG: &str = "ENC_CHANNEL_KEY_TAG:V1";
const ENC_SENDER_ADDR_TAG: &str = "ENC_SENDER_ADDR_TAG:V1";
const NOTE_ID_TAG: &str = "NOTE_ID_TAG:V1";
const ENC_AMOUNT_TAG: &str = "ENC_AMOUNT_TAG:V1";
const ENC_TOKEN_TAG: &str = "ENC_TOKEN_TAG:V1";
const ENC_PRIVATE_KEY_TAG: &str = "ENC_PRIVATE_KEY_TAG:V1";
const ENC_USER_ADDR_TAG: &str = "ENC_USER_ADDR_TAG:V1";
const ENC_RECIPIENT_ADDR_TAG: &str = "ENC_RECIPIENT_ADDR_TAG:V1";
const OUTGOING_CHANNEL_ID_TAG: &str = "OUTGOING_CHANNEL_ID_TAG:V1";
const IDENTITY_KEY_TAG: &str = "IDENTITY_KEY_TAG:V1";

/// A Cairo short string (≤ 31 ASCII bytes) as a felt.
pub fn short_string(s: &str) -> Felt {
    debug_assert!(s.len() <= 31 && s.is_ascii());
    Felt::from_bytes_be_slice(s.as_bytes())
}

fn hash(tag: &str, inputs: &[Felt]) -> Felt {
    let mut values = Vec::with_capacity(inputs.len() + 1);
    values.push(short_string(tag));
    values.extend_from_slice(inputs);
    Poseidon::hash_array(&values)
}

fn idx(index: u32) -> Felt {
    Felt::from(index)
}

pub fn identity_key(user_addr: Felt, user_private_key: Felt, contract_address: Felt) -> Felt {
    hash(
        IDENTITY_KEY_TAG,
        &[user_addr, user_private_key, contract_address],
    )
}

pub fn enc_private_key_hash(shared_x: Felt) -> Felt {
    hash(ENC_PRIVATE_KEY_TAG, &[shared_x])
}

pub fn enc_user_addr_hash(shared_x: Felt) -> Felt {
    hash(ENC_USER_ADDR_TAG, &[shared_x])
}

pub fn enc_token_hash(channel_key: Felt, index: u32, salt: Felt) -> Felt {
    hash(ENC_TOKEN_TAG, &[channel_key, idx(index), Felt::ZERO, salt])
}

pub fn enc_channel_key_hash(shared_x: Felt) -> Felt {
    hash(ENC_CHANNEL_KEY_TAG, &[shared_x])
}

pub fn enc_sender_addr_hash(shared_x: Felt) -> Felt {
    hash(ENC_SENDER_ADDR_TAG, &[shared_x])
}

pub fn enc_recipient_addr_hash(
    sender_addr: Felt,
    sender_private_key: Felt,
    index: u32,
    salt: Felt,
) -> Felt {
    hash(
        ENC_RECIPIENT_ADDR_TAG,
        &[
            sender_addr,
            sender_private_key,
            idx(index),
            Felt::ZERO,
            salt,
        ],
    )
}

pub fn channel_key(
    sender_addr: Felt,
    sender_private_key: Felt,
    recipient_addr: Felt,
    recipient_public_key: Felt,
) -> Felt {
    hash(
        CHANNEL_KEY_TAG,
        &[
            sender_addr,
            sender_private_key,
            recipient_addr,
            recipient_public_key,
        ],
    )
}

pub fn outgoing_channel_id(sender_addr: Felt, sender_private_key: Felt, index: u32) -> Felt {
    hash(
        OUTGOING_CHANNEL_ID_TAG,
        &[sender_addr, sender_private_key, idx(index), Felt::ZERO],
    )
}

pub fn channel_marker(
    channel_key: Felt,
    sender_addr: Felt,
    recipient_addr: Felt,
    recipient_public_key: Felt,
) -> Felt {
    hash(
        CHANNEL_MARKER_TAG,
        &[
            channel_key,
            sender_addr,
            recipient_addr,
            recipient_public_key,
        ],
    )
}

pub fn subchannel_id(channel_key: Felt, index: u32) -> Felt {
    hash(SUBCHANNEL_ID_TAG, &[channel_key, idx(index), Felt::ZERO])
}

pub fn subchannel_marker(
    channel_key: Felt,
    recipient_addr: Felt,
    recipient_public_key: Felt,
    token: Felt,
) -> Felt {
    hash(
        SUBCHANNEL_MARKER_TAG,
        &[channel_key, recipient_addr, recipient_public_key, token],
    )
}

pub fn note_id(channel_key: Felt, token: Felt, index: u32) -> Felt {
    hash(NOTE_ID_TAG, &[channel_key, token, idx(index), Felt::ZERO])
}

pub fn enc_amount_hash(channel_key: Felt, token: Felt, index: u32, salt: Felt) -> Felt {
    hash(
        ENC_AMOUNT_TAG,
        &[channel_key, token, idx(index), Felt::ZERO, salt],
    )
}

pub fn nullifier(channel_key: Felt, token: Felt, index: u32, owner_private_key: Felt) -> Felt {
    hash(
        NULLIFIER_TAG,
        &[
            channel_key,
            token,
            idx(index),
            Felt::ZERO,
            owner_private_key,
        ],
    )
}
