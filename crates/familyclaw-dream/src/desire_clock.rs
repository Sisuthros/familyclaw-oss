//! Desire Clock — yöllinen uniajastin (Anthropic §2.3.1).
//!
//! Tämä moduuli tarjoaa [`DesireClock`]in, joka määrittää kellonajat jolloin
//! [`familyclaw_dream::DreamCycle`] tulisi ajaa. Se noudattaa Anthropicin
//! Dreaming-mallin (design §2.3.1) mukaista käytäntöä:
//!
//! - **3AM-local reflection** — unijakso käynnistyy aina klo 03:00 paikka-aikavyöhykkeellä
//! - **Nightly cadence** — yksi unijakso yössä, ei päivällä
//! - **Missed runs** — jos unijakso jää väliin (esim. kone ei päällä), se
//!   ajetaan seuraavana mahdollisena hetkenä
//!
//! # Esimerkki
//! ```rust
//! use familyclaw_dream::desire_clock::{DesireClock, NextDream};
//! use chrono::{DateTime, Utc, TimeZone};
//!
//! let clock = DesireClock::default();
//! let now = Utc.with_ymd_and_hms(2026, 6, 4, 16, 30, 0).unwrap();
//! let next = clock.next_dream_time(now);
//! match next {
//!     NextDream::Tonight { at } => println!("Seuraava uni: {}", at),
//!     NextDream::Tomorrow { at } => println!("Uni huomenna: {}", at),
//!     NextDream::Missed { should_have_run_at } => println!("Unijakso jäänyt väliin: {}", should_have_run_at),
//! }
//! ```

use chrono::{DateTime, Datelike, TimeZone, Utc};
use familyclaw_core::Timestamp;

/// Desire Clock — Anthropicin yöllisen unimallin ajastin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesireClock {
    /// Uniajan kellonaika paikallisella aikavyöhykkeellä (esim. 03:00).
    reflection_hour: u32,
    /// Minuutti jolloin unijakso käynnistyy (esim. 00).
    reflection_minute: u32,
}

/// Seuraavan unijakson ajankohta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextDream {
    /// Unijakso käynnistyy tänään yöllä.
    Tonight {
        /// Ajankohta jolloin tämänyöinen unijakso käynnistyy.
        at: Timestamp,
    },
    /// Unijakso käynnistyy huomenna yöllä.
    Tomorrow {
        /// Ajankohta jolloin huomisyöinen unijakso käynnistyy.
        at: Timestamp,
    },
    /// Edellinen unijakso jäi ajamatta (kone ei päällä).
    Missed {
        /// Ajankohta jolloin unijakson olisi pitänyt käynnistyä.
        should_have_run_at: Timestamp,
    },
}

impl Default for DesireClock {
    fn default() -> Self {
        Self {
            reflection_hour: 3,   // 03:00
            reflection_minute: 0, // 00 minuuttia
        }
    }
}

impl DesireClock {
    /// Luo Desire Clockin annetulla kellonajalla.
    #[must_use]
    pub fn new(reflection_hour: u32, reflection_minute: u32) -> Self {
        Self {
            reflection_hour,
            reflection_minute,
        }
    }

    /// Laske seuraavan unijakson ajankohta nyt hetkeen nähden.
    ///
    /// Palauttaa:
    /// - [`NextDream::Tonight`] jos tämä yö ei ole vielä mennyt
    /// - [`NextDream::Tomorrow`] jos tämä yö on jo mennyt
    #[must_use]
    pub fn next_dream_time(&self, now: Timestamp) -> NextDream {
        let utc: DateTime<Utc> = now;
        let today_reflection = Utc
            .with_ymd_and_hms(
                utc.year(),
                utc.month(),
                utc.day(),
                self.reflection_hour,
                self.reflection_minute,
                0,
            )
            .unwrap();

        if utc < today_reflection {
            // Tänään yö ei ole vielä mennyt → unijakso tänä yönä
            NextDream::Tonight {
                at: today_reflection,
            }
        } else {
            // Tämä yö on mennyt → unijakso huomenna
            let tomorrow_reflection = today_reflection + chrono::Duration::days(1);
            NextDream::Tomorrow {
                at: tomorrow_reflection,
            }
        }
    }

    /// Laske viimeisin unijakson ajankohta nyt hetkeen nähden.
    ///
    /// Palauttaa:
    /// - [`NextDream::Missed`] jos tämä yö ei ole vielä mennyt (kone ei päällä eilen)
    /// - [`NextDream::Tonight`] jos tämä yö on mennyt (kone oli päällä tänään)
    #[must_use]
    pub fn last_dream_time(&self, now: Timestamp) -> NextDream {
        let utc: DateTime<Utc> = now;
        let today_reflection = Utc
            .with_ymd_and_hms(
                utc.year(),
                utc.month(),
                utc.day(),
                self.reflection_hour,
                self.reflection_minute,
                0,
            )
            .unwrap();

        if utc < today_reflection {
            // Tänään yö ei ole vielä mennyt → viimeisin unijakso oli eilen
            let yesterday_reflection = today_reflection - chrono::Duration::days(1);
            NextDream::Missed {
                should_have_run_at: yesterday_reflection,
            }
        } else {
            // Tämä yö on mennyt → viimeisin unijakso oli tänään
            NextDream::Tonight {
                at: today_reflection,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn next_dream_tonight_if_before_3am() {
        let clock = DesireClock::default();
        let now = Utc.with_ymd_and_hms(2026, 6, 4, 2, 15, 0).unwrap();
        let next = clock.next_dream_time(now);
        match next {
            NextDream::Tonight { at } => {
                let expected = Utc.with_ymd_and_hms(2026, 6, 4, 3, 0, 0).unwrap();
                assert_eq!(at, expected);
            }
            _ => panic!("odotettiin Tonight, saatiin {next:?}"),
        }
    }

    #[test]
    fn next_dream_tomorrow_if_after_3am() {
        let clock = DesireClock::default();
        let now = Utc.with_ymd_and_hms(2026, 6, 4, 16, 30, 0).unwrap();
        let next = clock.next_dream_time(now);
        match next {
            NextDream::Tomorrow { at } => {
                let expected = Utc.with_ymd_and_hms(2026, 6, 5, 3, 0, 0).unwrap();
                assert_eq!(at, expected);
            }
            _ => panic!("odotettiin Tomorrow, saatiin {next:?}"),
        }
    }

    #[test]
    fn last_dream_missed_if_before_3am() {
        let clock = DesireClock::default();
        let now = Utc.with_ymd_and_hms(2026, 6, 4, 2, 15, 0).unwrap();
        let last = clock.last_dream_time(now);
        match last {
            NextDream::Missed { should_have_run_at } => {
                let expected = Utc.with_ymd_and_hms(2026, 6, 3, 3, 0, 0).unwrap();
                assert_eq!(should_have_run_at, expected);
            }
            _ => panic!("odotettiin Missed, saatiin {last:?}"),
        }
    }

    #[test]
    fn last_dream_tonight_if_after_3am() {
        let clock = DesireClock::default();
        let now = Utc.with_ymd_and_hms(2026, 6, 4, 16, 30, 0).unwrap();
        let last = clock.last_dream_time(now);
        match last {
            NextDream::Tonight { at } => {
                let expected = Utc.with_ymd_and_hms(2026, 6, 4, 3, 0, 0).unwrap();
                assert_eq!(at, expected);
            }
            _ => panic!("odotettiin Tonight, saatiin {last:?}"),
        }
    }
}
