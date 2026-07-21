//! Turn watchdog: ensures every user message receives a reply or an error notification.
//!
//! The watchdog is a **soft deadline + late delivery** design, not a hard
//! drop-at-one-deadline design: past `turn_watchdog_secs()` the turn is not
//! killed — the user gets an interim "still working" notice while the turn
//! keeps running, and if it finishes before the hard cap
//! (`turn_watchdog_secs() * turn_watchdog_hard_multiplier()`), the real reply
//! is still delivered (late). Only past the hard cap is the turn actually
//! abandoned. This avoids discarding minutes of completed LLM work just
//! because a single turn ran a bit long.

use familyclaw_bus::BusMessage;

/// Default time limit in seconds for a single turn (`handle_turn_with_origin`)
/// before the soft (interim-notice) deadline fires.
pub const DEFAULT_TURN_WATCHDOG_SECS: u64 = 120;

/// Reads the `FAMILYCLAW_TURN_WATCHDOG_SECS` environment variable, or returns the default.
#[must_use]
pub fn turn_watchdog_secs() -> u64 {
    std::env::var("FAMILYCLAW_TURN_WATCHDOG_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_TURN_WATCHDOG_SECS)
}

/// Default multiplier applied to `turn_watchdog_secs()` to get the hard cap:
/// the point past which the turn is truly abandoned (future dropped) and
/// [`WATCHDOG_TIMEOUT_MSG`] is sent, no matter how far along it might be.
pub const DEFAULT_TURN_WATCHDOG_HARD_MULTIPLIER: u64 = 3;

/// Reads the `FAMILYCLAW_TURN_WATCHDOG_HARD_MULTIPLIER` environment variable,
/// or returns the default. Values below `1` are rejected (fall back to the
/// default) so the hard cap can never be shorter than the soft deadline.
#[must_use]
pub fn turn_watchdog_hard_multiplier() -> u64 {
    std::env::var("FAMILYCLAW_TURN_WATCHDOG_HARD_MULTIPLIER")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n: &u64| n >= 1)
        .unwrap_or(DEFAULT_TURN_WATCHDOG_HARD_MULTIPLIER)
}

/// Returns the hard cap in seconds given a soft deadline
/// (`watchdog_secs * turn_watchdog_hard_multiplier()`).
#[must_use]
pub fn turn_watchdog_hard_secs(watchdog_secs: u64) -> u64 {
    watchdog_secs.saturating_mul(turn_watchdog_hard_multiplier())
}

/// Message shown when a turn gets stuck past the time limit (usually the LLM chain is too slow).
pub const WATCHDOG_TIMEOUT_MSG: &str =
    "LLM-vastaus kesti liian kauan (ketju timeout). Lyhennä kysymystä tai kokeile uudelleen — \
     jos toistuu, tarkista FAMILYCLAW_PROVIDER_MODEL / fallback-mallit gateway-lokista.";

/// Interim notice sent once a turn passes the *soft* deadline but is still
/// running. `{hard}` is replaced with the hard-cap second count by
/// [`watchdog_still_working_msg`] — use that function rather than this
/// constant directly.
pub const WATCHDOG_STILL_WORKING_MSG: &str =
    "Työstän vastausta yhä — ketju on hidas mutta etenee. Toimitan vastauksen kun se \
     valmistuu (kova raja {hard}s).";

/// Formats [`WATCHDOG_STILL_WORKING_MSG`] with the concrete hard-cap second count.
#[must_use]
pub fn watchdog_still_working_msg(hard_secs: u64) -> String {
    WATCHDOG_STILL_WORKING_MSG.replace("{hard}", &hard_secs.to_string())
}

/// Message shown when handling the turn returns an error.
pub const WATCHDOG_ERROR_MSG: &str =
    "Vuoron käsittely epäonnistui — yritä uudelleen hetken kuluttua.";

/// Message shown when the turn completed but no reply was sent to the user (turn-91 class).
pub const WATCHDOG_SILENCE_MSG: &str =
    "Sain viestisi mutta vastaus jäi puuttumaan — yritän uudelleen.";

/// Returns `true` if the bus message is a user conversation awaiting a reply.
#[must_use]
pub fn message_expects_user_reply(message: &BusMessage) -> bool {
    matches!(message, BusMessage::Text { .. } | BusMessage::Latent { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    // Env vars are process-global; serialize tests that touch them so they
    // don't race each other (same pattern as `identity.rs`).
    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn env_test_lock() -> MutexGuard<'static, ()> {
        ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// RAII guard: sets `key` to `value` on construction, restores whatever
    /// was there before on drop (even on panic, so tests can't poison env
    /// state for later tests in this module).
    struct EnvVarGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prior = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prior }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.prior.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn hard_multiplier_defaults_when_unset() {
        const ENV: &str = "FAMILYCLAW_TURN_WATCHDOG_HARD_MULTIPLIER";
        let _lock = env_test_lock();
        std::env::remove_var(ENV);
        assert_eq!(
            turn_watchdog_hard_multiplier(),
            DEFAULT_TURN_WATCHDOG_HARD_MULTIPLIER
        );
    }

    #[test]
    fn hard_multiplier_reads_valid_override() {
        const ENV: &str = "FAMILYCLAW_TURN_WATCHDOG_HARD_MULTIPLIER";
        let _lock = env_test_lock();
        let _guard = EnvVarGuard::set(ENV, "5");
        assert_eq!(turn_watchdog_hard_multiplier(), 5);
    }

    #[test]
    fn hard_multiplier_rejects_zero_and_garbage() {
        const ENV: &str = "FAMILYCLAW_TURN_WATCHDOG_HARD_MULTIPLIER";
        let _lock = env_test_lock();
        let _guard = EnvVarGuard::set(ENV, "0");
        assert_eq!(
            turn_watchdog_hard_multiplier(),
            DEFAULT_TURN_WATCHDOG_HARD_MULTIPLIER,
            "0 must fall back to the default (hard cap can't be < soft deadline)"
        );
        let _guard = EnvVarGuard::set(ENV, "not-a-number");
        assert_eq!(
            turn_watchdog_hard_multiplier(),
            DEFAULT_TURN_WATCHDOG_HARD_MULTIPLIER
        );
    }

    #[test]
    fn hard_secs_multiplies_soft_deadline() {
        const ENV: &str = "FAMILYCLAW_TURN_WATCHDOG_HARD_MULTIPLIER";
        let _lock = env_test_lock();
        let _guard = EnvVarGuard::set(ENV, "3");
        assert_eq!(turn_watchdog_hard_secs(80), 240);
        let _guard2 = EnvVarGuard::set(ENV, "1");
        assert_eq!(
            turn_watchdog_hard_secs(80),
            80,
            "multiplier of 1 = hard == soft"
        );
    }

    #[test]
    fn still_working_msg_substitutes_hard_secs_and_is_finnish() {
        let msg = watchdog_still_working_msg(240);
        assert!(msg.contains("240s"));
        assert!(!msg.contains("{hard}"), "placeholder must be substituted");
        assert!(
            msg.contains("Työstän"),
            "message should be in Finnish, matching the rest of watchdog.rs"
        );
    }
}
