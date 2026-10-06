//! Client actions: the wallet's input to the pool's `compile_actions`.
//!
//! These mirror `privacy::actions::ClientAction` in the pool's ABI. Every field
//! is a single felt on the wire (`ContractAddress`, `felt252`, `u32` and `u128`
//! all serialize to one felt), and an action serializes as
//! `[variant_index, ...fields]`. The pool runs actions in a fixed phase order
//! (register, channels, subchannels, deposits, spends, new notes, withdrawals,
//! invoke), so callers must emit them in that order; [`Phase`] encodes it.
//!
//! `InvokeExternal` and `ComputeAndInvoke` are not supported yet
//! (`docs/project/strk20-plan.md` §3, "Later").

use starknet_types_core::felt::Felt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientAction {
    /// Register the account's viewing key. Immutable once set.
    SetViewingKey { random: Felt },
    /// Open a channel from the user to `recipient_addr`. `index` is the user's
    /// outgoing-channel count.
    OpenChannel {
        recipient_addr: Felt,
        index: u32,
        random: Felt,
        salt: Felt,
    },
    /// Open a token subchannel inside an existing channel. `index` is the
    /// channel's subchannel count.
    OpenSubchannel {
        recipient_addr: Felt,
        recipient_public_key: Felt,
        channel_key: Felt,
        index: u32,
        token: Felt,
        salt: Felt,
    },
    /// Create an encrypted note. `index` is the subchannel's next note index;
    /// `salt` must be ≥ 2 (1 marks an open note) and fit in 120 bits.
    CreateEncNote {
        recipient_addr: Felt,
        recipient_public_key: Felt,
        token: Felt,
        amount: u128,
        index: u32,
        salt: u128,
    },
    /// Pull `amount` of `token` from the user's public balance into the pool.
    Deposit { token: Felt, amount: u128 },
    /// Spend a note (publishes its nullifier).
    UseNote {
        channel_key: Felt,
        token: Felt,
        index: u32,
    },
    /// Send `amount` of `token` out of the pool to `to_addr`.
    Withdraw {
        to_addr: Felt,
        token: Felt,
        amount: u128,
        random: Felt,
    },
}

/// The pool's execution phases, in order (`ClientActionTrait::phase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    Account = 0,
    Channel = 1,
    Subchannel = 2,
    Deposit = 3,
    UseNotes = 4,
    CreateNotes = 5,
    Withdraw = 6,
}

impl ClientAction {
    /// Index of the variant in `privacy::actions::ClientAction`.
    fn variant(&self) -> u64 {
        match self {
            ClientAction::SetViewingKey { .. } => 0,
            ClientAction::OpenChannel { .. } => 1,
            ClientAction::OpenSubchannel { .. } => 2,
            ClientAction::CreateEncNote { .. } => 3,
            // 4 = CreateOpenNote (unsupported)
            ClientAction::Deposit { .. } => 5,
            ClientAction::UseNote { .. } => 6,
            ClientAction::Withdraw { .. } => 7,
            // 8 = InvokeExternal, 9 = ComputeAndInvoke (unsupported)
        }
    }

    pub fn phase(&self) -> Phase {
        match self {
            ClientAction::SetViewingKey { .. } => Phase::Account,
            ClientAction::OpenChannel { .. } => Phase::Channel,
            ClientAction::OpenSubchannel { .. } => Phase::Subchannel,
            ClientAction::Deposit { .. } => Phase::Deposit,
            ClientAction::UseNote { .. } => Phase::UseNotes,
            ClientAction::CreateEncNote { .. } => Phase::CreateNotes,
            ClientAction::Withdraw { .. } => Phase::Withdraw,
        }
    }

    /// Cairo serde: `[variant_index, ...fields]`.
    pub fn serialize(&self, out: &mut Vec<Felt>) {
        out.push(Felt::from(self.variant()));
        match *self {
            ClientAction::SetViewingKey { random } => out.push(random),
            ClientAction::OpenChannel {
                recipient_addr,
                index,
                random,
                salt,
            } => out.extend([recipient_addr, Felt::from(index), random, salt]),
            ClientAction::OpenSubchannel {
                recipient_addr,
                recipient_public_key,
                channel_key,
                index,
                token,
                salt,
            } => out.extend([
                recipient_addr,
                recipient_public_key,
                channel_key,
                Felt::from(index),
                token,
                salt,
            ]),
            ClientAction::CreateEncNote {
                recipient_addr,
                recipient_public_key,
                token,
                amount,
                index,
                salt,
            } => out.extend([
                recipient_addr,
                recipient_public_key,
                token,
                Felt::from(amount),
                Felt::from(index),
                Felt::from(salt),
            ]),
            ClientAction::Deposit { token, amount } => out.extend([token, Felt::from(amount)]),
            ClientAction::UseNote {
                channel_key,
                token,
                index,
            } => out.extend([channel_key, token, Felt::from(index)]),
            ClientAction::Withdraw {
                to_addr,
                token,
                amount,
                random,
            } => out.extend([to_addr, token, Felt::from(amount), random]),
        }
    }
}

/// Serialize a `Span<ClientAction>`: length prefix, then each action.
pub fn serialize_actions(actions: &[ClientAction]) -> Vec<Felt> {
    let mut out = vec![Felt::from(actions.len() as u64)];
    for action in actions {
        action.serialize(&mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_variant_then_fields() {
        let actions = [
            ClientAction::SetViewingKey {
                random: Felt::from(9u64),
            },
            ClientAction::Deposit {
                token: Felt::from(0x4718u64),
                amount: 10u128.pow(18),
            },
        ];
        assert_eq!(
            serialize_actions(&actions),
            vec![
                Felt::TWO,
                Felt::ZERO,
                Felt::from(9u64),
                Felt::from(5u64),
                Felt::from(0x4718u64),
                Felt::from(10u128.pow(18)),
            ]
        );
    }

    #[test]
    fn phases_are_ordered_like_the_pool() {
        assert!(Phase::Account < Phase::Channel);
        assert!(Phase::Deposit < Phase::UseNotes);
        assert!(Phase::CreateNotes < Phase::Withdraw);
    }
}
