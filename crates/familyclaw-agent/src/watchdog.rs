//! Turn watchdog: ensures every user message receives a reply or an error notification.

use familyclaw_bus::BusMessage;

/// Default time limit in seconds for a single turn (`handle_turn_with_origin`).
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

/// Message shown when a turn gets stuck past the time limit (usually the LLM chain is too slow).
pub const WATCHDOG_TIMEOUT_MSG: &str =
    "LLM-vastaus kesti liian kauan (ketju timeout). Lyhennä kysymystä tai kokeile uudelleen — \
     jos toistuu, tarkista FAMILYCLAW_PROVIDER_MODEL / fallback-mallit gateway-lokista.";

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
