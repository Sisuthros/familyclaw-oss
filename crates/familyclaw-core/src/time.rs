//! Ajan apufunktiot [`chrono`]-pohjalta.
//!
//! Alusta käyttää johdonmukaisesti UTC-aikaa ([`Timestamp`]) sisäisessä
//! tilassa ja sarjallistuksessa, jotta durable-replay ja muistin
//! konsolidointi pysyvät deterministisinä aikavyöhykkeestä riippumatta.
//! Paikallisaika lasketaan vain esitystä varten.

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};

/// Alustan kanoninen aikaleima — aina UTC.
pub type Timestamp = DateTime<Utc>;

/// Palauttaa nykyhetken UTC-aikaleimana.
#[must_use]
pub fn now() -> Timestamp {
    Utc::now()
}

/// Muotoilee aikaleiman RFC 3339 -muotoon millisekuntien tarkkuudella.
///
/// Vakiotarkkuus tekee lokeista ja sarjallistuksesta vertailukelpoisia.
#[must_use]
pub fn to_rfc3339(ts: Timestamp) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Jäsentää RFC 3339 -aikaleiman UTC:ksi.
///
/// # Errors
/// Palauttaa [`crate::FamilyClawError::InvalidInput`] jos merkkijono ei ole
/// kelvollinen RFC 3339 -aikaleima.
pub fn parse_rfc3339(s: &str) -> crate::Result<Timestamp> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            crate::FamilyClawError::invalid_input(format!("invalid rfc3339 timestamp: {e}"))
        })
}

/// Rakentaa aikaleiman Unix-sekunneista (UTC).
///
/// # Errors
/// Palauttaa [`crate::FamilyClawError::InvalidInput`] jos sekuntiarvo on
/// kelpaamaton (esim. ylivuoto).
pub fn from_unix_secs(secs: i64) -> crate::Result<Timestamp> {
    match Utc.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(ts) => Ok(ts),
        _ => Err(crate::FamilyClawError::invalid_input(format!(
            "invalid unix seconds: {secs}"
        ))),
    }
}

/// Palauttaa aikaleiman Unix-sekunteina (UTC).
#[must_use]
pub fn to_unix_secs(ts: Timestamp) -> i64 {
    ts.timestamp()
}

/// Onko `ts` annettua kestoa vanhempi suhteessa nykyhetkeen.
///
/// Negatiivinen kesto käsitellään nollana (tulevaisuus ei ole "vanhentunut").
#[must_use]
pub fn is_older_than(ts: Timestamp, age: chrono::Duration) -> bool {
    now().signed_duration_since(ts) > age
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_recent() {
        let before = Utc::now();
        let n = now();
        let after = Utc::now();
        assert!(n >= before && n <= after);
    }

    #[test]
    fn rfc3339_roundtrip_preserves_instant() {
        let ts = from_unix_secs(1_717_000_000).expect("valid unix seconds");
        let text = to_rfc3339(ts);
        let parsed = parse_rfc3339(&text).expect("roundtrip parse");
        assert_eq!(ts, parsed);
    }

    #[test]
    fn rfc3339_uses_millis_and_z() {
        let ts = from_unix_secs(0).expect("epoch is valid");
        let text = to_rfc3339(ts);
        assert_eq!(text, "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn parse_rejects_garbage() {
        let err = parse_rfc3339("not a date").expect_err("garbage must fail");
        assert!(matches!(err, crate::FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn unix_secs_roundtrip() {
        let secs = 1_700_123_456_i64;
        let ts = from_unix_secs(secs).expect("valid");
        assert_eq!(to_unix_secs(ts), secs);
    }

    #[test]
    fn is_older_than_detects_past_and_future() {
        let past = now() - chrono::Duration::hours(2);
        assert!(is_older_than(past, chrono::Duration::hours(1)));
        assert!(!is_older_than(past, chrono::Duration::hours(3)));

        let future = now() + chrono::Duration::hours(2);
        assert!(!is_older_than(future, chrono::Duration::hours(1)));
    }
}
