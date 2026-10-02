//! Escalating delay after wrong passphrases.
//!
//! Each guess already costs an Argon2id derivation (~0.1–0.3 s). This adds a
//! growing wait after a few misses, so anything that can call the desktop's IPC
//! (a compromised webview, an injected script) can't grind passphrases at that
//! rate. It is defence in depth: someone holding the vault file can still guess
//! offline, which the KDF, not this, has to make expensive.

use std::time::{Duration, Instant};

/// Wrong passphrases allowed before any delay: typos shouldn't be punished.
pub const FREE_ATTEMPTS: u32 = 3;
/// Delay after the first miss beyond the free ones; doubles after each miss.
pub const BASE_DELAY: Duration = Duration::from_secs(5);
/// Longest single delay.
pub const MAX_DELAY: Duration = Duration::from_secs(300);

#[derive(Debug, Default)]
pub struct PassphraseBackoff {
    failures: u32,
    until: Option<Instant>,
}

impl PassphraseBackoff {
    /// `Err(wait)` while a delay is in force.
    pub fn check(&self, now: Instant) -> Result<(), Duration> {
        match self.until {
            Some(t) if now < t => Err(t - now),
            _ => Ok(()),
        }
    }

    /// Count a wrong passphrase and start the next delay, if one is due.
    pub fn record_failure(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        if self.failures > FREE_ATTEMPTS {
            let doublings = (self.failures - FREE_ATTEMPTS - 1).min(16);
            let delay = BASE_DELAY.saturating_mul(1u32 << doublings).min(MAX_DELAY);
            self.until = Some(now + delay);
        }
    }

    /// A right passphrase clears the history.
    pub fn record_success(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_misses_are_free_then_the_delay_doubles_up_to_the_cap() {
        let t0 = Instant::now();
        let mut b = PassphraseBackoff::default();
        for _ in 0..FREE_ATTEMPTS {
            b.record_failure(t0);
            assert!(b.check(t0).is_ok(), "typos within the free attempts are not delayed");
        }
        b.record_failure(t0);
        assert_eq!(b.check(t0), Err(BASE_DELAY));
        b.record_failure(t0);
        assert_eq!(b.check(t0), Err(BASE_DELAY * 2));
        for _ in 0..20 {
            b.record_failure(t0);
        }
        assert_eq!(b.check(t0), Err(MAX_DELAY));
        assert!(b.check(t0 + MAX_DELAY).is_ok(), "the delay expires");
    }

    #[test]
    fn a_right_passphrase_resets_it() {
        let t0 = Instant::now();
        let mut b = PassphraseBackoff::default();
        for _ in 0..(FREE_ATTEMPTS + 2) {
            b.record_failure(t0);
        }
        assert!(b.check(t0).is_err());
        b.record_success();
        assert!(b.check(t0).is_ok());
        b.record_failure(t0);
        assert!(b.check(t0).is_ok(), "the free attempts are restored");
    }
}
