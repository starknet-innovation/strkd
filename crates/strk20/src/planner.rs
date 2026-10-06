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
    /// Privacy warnings to surface before signing.
    pub warnings: Vec<Warning>,
}

/// The SDK's privacy warnings (`WarningCode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Warning {
    /// More than one channel opened in one batch: an observer can tell the new
    /// channels share a sender (SDK `USER_LINKAGE`).
    UserLinkage,
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

    let opened = actions
        .iter()
        .filter(|a| matches!(a, ClientAction::OpenChannel { .. }))
        .count();
    let warnings = if opened > 1 {
        vec![Warning::UserLinkage]
    } else {
        Vec::new()
    };
    Ok(Plan {
        actions,
        spent: spends.iter().map(|n| n.id).collect(),
        change,
        warnings,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic randomness: counts up from 2.
    struct Counter(u64);
    impl Randomness for Counter {
        fn felt(&mut self) -> Result<Felt, Error> {
            self.0 += 1;
            Ok(Felt::from(self.0))
        }
        fn salt120(&mut self) -> Result<u128, Error> {
            self.0 += 1;
            Ok(self.0 as u128)
        }
    }

    const USER: u64 = 0xa11ce;
    const BOB: u64 = 0xb0b;
    const STRK: u64 = 0x4718;
    const SK: u64 = 0x5ec;

    fn channel(recipient: u64, open: bool, subs: &[(u64, u32, u32)]) -> OutgoingChannel {
        OutgoingChannel {
            recipient: Felt::from(recipient),
            recipient_public_key: Felt::from(recipient + 1),
            key: Felt::from(recipient + 2),
            open,
            subchannels: subs
                .iter()
                .map(|&(t, index, next)| {
                    (
                        Felt::from(t),
                        crate::discovery::Subchannel {
                            index,
                            next_note_index: next,
                        },
                    )
                })
                .collect(),
        }
    }

    fn note(amount: u128, index: u32) -> Note {
        Note {
            id: Felt::from(1000 + index as u64),
            token: Felt::from(STRK),
            amount,
            channel_key: Felt::from(77u64),
            index,
            sender: Felt::from(USER),
            open: false,
        }
    }

    fn state(registered: bool, channels: Vec<OutgoingChannel>, notes: Vec<Note>) -> State {
        State {
            user: Felt::from(USER),
            user_sk: Felt::from(SK),
            registered,
            outgoing_channels: channels.iter().filter(|c| c.open).count() as u32,
            channels: channels.into_iter().map(|c| (c.recipient, c)).collect(),
            notes,
        }
    }

    #[test]
    fn register_and_deposit_open_the_self_channel_and_subchannel() {
        let intent = Intent {
            register: true,
            deposits: vec![(Felt::from(STRK), 5)],
            ..Intent::default()
        };
        let p = plan(&intent, &state(false, vec![], vec![]), &mut Counter(1)).unwrap();
        let kinds: Vec<_> = p.actions.iter().map(|a| a.phase()).collect();
        use crate::actions::Phase::*;
        assert_eq!(
            kinds,
            vec![Account, Channel, Subchannel, Deposit, CreateNotes]
        );
        assert!(matches!(
            p.actions[1],
            ClientAction::OpenChannel { index: 0, .. }
        ));
        assert!(matches!(
            p.actions[4],
            ClientAction::CreateEncNote {
                amount: 5,
                index: 0,
                ..
            }
        ));
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn transfer_spends_largest_notes_first_and_returns_change() {
        let st = state(
            true,
            vec![
                channel(USER, true, &[(STRK, 0, 3)]),
                channel(BOB, false, &[]),
            ],
            vec![note(10, 0), note(50, 1), note(30, 2)],
        );
        let intent = Intent {
            transfers: vec![(Felt::from(BOB), Felt::from(STRK), 70)],
            ..Intent::default()
        };
        let p = plan(&intent, &st, &mut Counter(1)).unwrap();
        assert_eq!(p.spent, vec![Felt::from(1001u64), Felt::from(1002u64)]); // 50, then 30
        assert_eq!(p.change.get(&Felt::from(STRK)), Some(&10));
        // Bob's channel opens at the user's next index (1); his note is the first on a new subchannel;
        // the change is the 4th note on the user's existing STRK subchannel.
        assert!(p.actions.contains(&ClientAction::OpenChannel {
            recipient_addr: Felt::from(BOB),
            index: 1,
            random: Felt::from(2u64),
            salt: Felt::from(3u64)
        }));
        let notes: Vec<_> = p
            .actions
            .iter()
            .filter_map(|a| match a {
                ClientAction::CreateEncNote {
                    recipient_addr,
                    amount,
                    index,
                    ..
                } => Some((*recipient_addr, *amount, *index)),
                _ => None,
            })
            .collect();
        assert_eq!(
            notes,
            vec![(Felt::from(BOB), 70, 0), (Felt::from(USER), 10, 3)]
        );
    }

    #[test]
    fn insufficient_balance_is_refused() {
        let st = state(
            true,
            vec![channel(USER, true, &[(STRK, 0, 1)])],
            vec![note(10, 0)],
        );
        let intent = Intent {
            withdrawals: vec![(Felt::from(USER), Felt::from(STRK), 11)],
            ..Intent::default()
        };
        assert!(matches!(
            plan(&intent, &st, &mut Counter(1)),
            Err(Error::InsufficientBalance {
                shortfall: 1,
                available: 10,
                ..
            })
        ));
    }

    #[test]
    fn unregistered_user_must_register_first() {
        let intent = Intent {
            deposits: vec![(Felt::from(STRK), 1)],
            ..Intent::default()
        };
        assert!(matches!(
            plan(&intent, &state(false, vec![], vec![]), &mut Counter(1)),
            Err(Error::NotRegistered(_))
        ));
    }

    #[test]
    fn opening_two_channels_warns_of_linkage() {
        let st = state(
            true,
            vec![channel(USER, false, &[]), channel(BOB, false, &[])],
            vec![note(5, 0)],
        );
        let intent = Intent {
            transfers: vec![(Felt::from(BOB), Felt::from(STRK), 2)],
            ..Intent::default()
        };
        assert_eq!(
            plan(&intent, &st, &mut Counter(1)).unwrap().warnings,
            vec![Warning::UserLinkage]
        );
    }
}
