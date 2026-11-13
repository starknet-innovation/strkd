//! `companion_strk20ChannelKey` (#49): disclose the key of one outgoing
//! channel, so a third party can verify notes the subject sent on it.
//!
//! The key is `channel_key(S, S_sk, R, R_pk)` (`strk20::hashes`). It is fixed
//! for the (sender, recipient) pair: whoever holds it can find and decrypt
//! every note S has sent or will ever send R, in every token, and tag the
//! transactions that carried them. It cannot spend, see whether a note was
//! spent, or find S's other channels. There is no narrower per-note secret
//! (`docs/project/design-49-strk20-channel-disclosure.md` §3), so this method
//! is worded and gated as a channel disclosure:
//!
//! - it always prompts (`Caller::approve_always`): a grant never covers it;
//! - it refuses the subject's channel to itself (`SELF_CHANNEL`) and a channel
//!   that does not exist yet (`CHANNEL_NOT_FOUND`);
//! - it never returns the outgoing-channel index (how many recipients S has);
//! - the request log redacts `channel_key` from the result (`dispatch.rs`,
//!   `redact_for_log`), and errors are scrubbed of the viewing key and the
//!   channel key.

use serde_json::{json, Value};
use strk20::{discovery, hashes};
use wallet_core::{address_hex, Felt};

use super::plan::subject_text;
use super::{address_param, ctx, hex, invalid, map_strk20, scrub, Caller};
use crate::dispatch::{chain_name, ServerState};
use crate::error::WalletRpcError;

pub const METHOD: &str = "companion_strk20ChannelKey";

/// A node or pool error with the viewing key and the channel key removed.
fn scrubbed(e: WalletRpcError, user_sk: Felt, channel_key: Option<Felt>) -> WalletRpcError {
    let clean = |m: &str| {
        let m = scrub(m, user_sk);
        match channel_key {
            Some(k) => m
                .replace(&format!("0x{k:064x}"), "<channel key>")
                .replace(&format!("{k:#x}"), "<channel key>")
                .replace(&k.to_string(), "<channel key>"),
            None => m,
        }
    };
    match e {
        WalletRpcError::Node(m) => WalletRpcError::Node(clean(&m)),
        WalletRpcError::Unknown(m) => WalletRpcError::Unknown(clean(&m)),
        e => e,
    }
}

/// The approval prompt. It must say plainly what is disclosed: every past and
/// future payment on the channel, in every token, irrevocably; and what is not.
fn prompt(
    subject: &str,
    sender: &str,
    recipient: &str,
    chain: &str,
    pool: &str,
    notes: u32,
    tokens: usize,
) -> String {
    format!(
        "Reveal the STRK20 channel key from {subject} to {recipient} on {chain} ({pool}). Whoever receives this \
         key can see EVERY private payment {sender} has sent or will ever send to {recipient}, in every token: \
         the tokens, the amounts, and the transactions that carried them (including any public withdrawals in \
         those transactions). This cannot be revoked: the key is fixed for this sender and recipient. It does not \
         let anyone spend these funds or send from this address, and it does not reveal payments to other \
         recipients. Already on this channel: {notes} note(s) in {tokens} token(s)."
    )
}

/// `companion_strk20ChannelKey { recipient, account_address? | owner_address?,
/// chainId?, pool_address? }` → `{ channel_key, sender, recipient,
/// recipient_public_key, marker, pool, chainId, subchannels: [{ token,
/// note_count }] }`.
///
/// `marker` is what the pool's `channel_exists(marker)` checks:
/// `H(CHANNEL_MARKER_TAG, channel_key, sender, recipient, recipient_public_key)`.
/// Note `i` of token `T` on the channel is `get_note(note_id(channel_key, T, i))`.
pub async fn channel_key(
    state: &ServerState,
    caller: Caller<'_>,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let recipient = address_param(params, "recipient")?
        .ok_or_else(|| invalid("missing address param 'recipient'"))?;
    let recipient =
        Felt::from_hex(&recipient).map_err(|_| invalid("recipient: expected a felt"))?;
    let ctx = ctx(state, caller, params).await?;
    if recipient == ctx.user {
        return Err(invalid(
            "recipient is the sender itself: its channel to itself carries all of its change and deposit notes, \
             so strkd never discloses it",
        )
        .with_reason("SELF_CHANNEL"));
    }

    let reader = ctx.reader(json!("latest"));
    let user_sk = ctx.viewing_key(state).await?;
    let recipient_pk = discovery::public_key(&reader, recipient)
        .await
        .map_err(|e| scrubbed(map_strk20(e), user_sk, None))?
        .ok_or_else(|| WalletRpcError::Strk20NotRegistered(address_hex(&recipient)))?;
    let key = hashes::channel_key(ctx.user, user_sk, recipient, recipient_pk);
    let channel = discovery::discover_channel(&reader, ctx.user, user_sk, recipient)
        .await
        .map_err(|e| scrubbed(map_strk20(e), user_sk, Some(key)))?;
    if !channel.open {
        return Err(WalletRpcError::Precondition(format!(
            "{} has no private channel to {} on this pool yet, so there is nothing to verify: send a private \
             transfer first",
            ctx.address(),
            address_hex(&recipient)
        ))
        .with_reason("CHANNEL_NOT_FOUND"));
    }
    let marker = hashes::channel_marker(key, ctx.user, recipient, recipient_pk);

    let mut subchannels: Vec<_> = channel.subchannels.iter().collect();
    subchannels.sort_by_key(|(_, s)| s.index);
    let notes: u32 = subchannels.iter().map(|(_, s)| s.next_note_index).sum();
    let text = prompt(
        &subject_text(&ctx),
        &ctx.address(),
        &address_hex(&recipient),
        chain_name(ctx.chain),
        &ctx.pool_label(),
        notes,
        subchannels.len(),
    );
    caller.approve_always(state, METHOD, text).await?;

    Ok(json!({
        "channel_key": hex(&key),
        "sender": ctx.address(),
        "recipient": address_hex(&recipient),
        "recipient_public_key": hex(&recipient_pk),
        "marker": hex(&marker),
        "pool": address_hex(&ctx.pool),
        "chainId": chain_name(ctx.chain),
        "subchannels": subchannels
            .iter()
            .map(|(token, s)| json!({ "token": hex(token), "note_count": s.next_note_index }))
            .collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_states_the_full_scope_of_the_disclosure() {
        let p = prompt(
            "your account 0xa",
            "0xa",
            "0xb",
            "SN_SEPOLIA",
            "pool 0x1",
            3,
            2,
        );
        for want in [
            "EVERY private payment",
            "has sent or will ever send",
            "in every token",
            "cannot be revoked",
            "does not let anyone spend",
            "does not reveal payments to other recipients",
            "3 note(s) in 2 token(s)",
            "SN_SEPOLIA",
            "pool 0x1",
        ] {
            assert!(p.contains(want), "prompt lacks {want:?}");
        }
    }

    #[test]
    fn errors_are_scrubbed_of_both_keys() {
        let (sk, ck) = (Felt::from(0x5ec_u128 << 100), Felt::from(0xc4_u128 << 112));
        let e = WalletRpcError::Node(format!("calldata [{sk:#x}, {ck:#x}, 0x{ck:064x}, {ck}]"));
        let WalletRpcError::Node(m) = scrubbed(e, sk, Some(ck)) else {
            panic!("not a node error")
        };
        assert!(!m.contains(&format!("{ck:#x}")[2..]) && !m.contains(&ck.to_string()));
        assert!(!m.contains(&format!("{sk:#x}")[2..]));
        assert_eq!(m.matches("<channel key>").count(), 3);
    }
}
