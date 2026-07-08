//! Turn-watchdog: varmistaa että jokainen käyttäjäviesti saa vastauksen tai virheilmoituksen.

use familyclaw_bus::BusMessage;

/// Oletus-aikaraja sekunteina yhdelle vuorolle (`handle_turn_with_origin`).
pub const DEFAULT_TURN_WATCHDOG_SECS: u64 = 120;

/// Lukee `FAMILYCLAW_TURN_WATCHDOG_SECS`-ympäristömuuttujan tai palauttaa oletuksen.
#[must_use]
pub fn turn_watchdog_secs() -> u64 {
    std::env::var("FAMILYCLAW_TURN_WATCHDOG_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_TURN_WATCHDOG_SECS)
}

/// Viesti kun vuoro jää jumiin aikarajassa (yleensä LLM-ketju liian hidas).
pub const WATCHDOG_TIMEOUT_MSG: &str =
    "LLM-vastaus kesti liian kauan (ketju timeout). Lyhennä kysymystä tai kokeile uudelleen — \
     jos toistuu, tarkista FAMILYCLAW_PROVIDER_MODEL / fallback-mallit gateway-lokista.";

/// Viesti kun vuoron käsittely palauttaa virheen.
pub const WATCHDOG_ERROR_MSG: &str =
    "Vuoron käsittely epäonnistui — yritä uudelleen hetken kuluttua.";

/// Viesti kun vuoro valmistui mutta käyttäjälle ei lähtenyt vastausta (turn-91-luokka).
pub const WATCHDOG_SILENCE_MSG: &str =
    "Sain viestisi mutta vastaus jäi puuttumaan — yritän uudelleen.";

/// Palauttaa `true` jos bus-viesti on käyttäjän keskustelu joka odottaa vastausta.
#[must_use]
pub fn message_expects_user_reply(message: &BusMessage) -> bool {
    matches!(message, BusMessage::Text { .. } | BusMessage::Latent { .. })
}
