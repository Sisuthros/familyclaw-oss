//! Desire Clock — nightly dream scheduler (Anthropic §2.3.1).
//!
//! This module provides [`DesireClock`], which determines the times at
//! which [`crate::DreamCycle`] should run. It follows the policy from
//! Anthropic's Dreaming model (design §2.3.1):
//!
//! - **3AM-local reflection** — the dream cycle always starts at 03:00 local time
//! - **Nightly cadence** — one dream cycle per night, never during the day
//! - **Missed runs** — if a dream cycle is missed (e.g. the machine was
//!   off), it runs at the next possible opportunity
//!
//! # Example
//! ```rust
//! use familyclaw_dream::desire_clock::{DesireClock, NextDream};
//! use chrono::{DateTime, Utc, TimeZone};
//!
//! let clock = DesireClock::default();
//! let now = Utc.with_ymd_and_hms(2026, 6, 4, 16, 30, 0).unwrap();
//! let next = clock.next_dream_time(now);
//! match next {
//!     NextDream::Tonight { at } => println!("Next dream: {}", at),
//!     NextDream::Tomorrow { at } => println!("Dream tomorrow: {}", at),
//!     NextDream::Missed { should_have_run_at } => println!("Dream cycle missed: {}", should_have_run_at),
//! }
//! ```

use chrono::{DateTime, Datelike, TimeZone, Utc};
use familyclaw_core::Timestamp;

/// Desire Clock — Anthropic's nightly dream model scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesireClock {
    /// Reflection time-of-day in local time zone (e.g. 03:00).
    reflection_hour: u32,
    /// Minute at which the dream cycle starts (e.g. 00).
    reflection_minute: u32,
}

/// The time of the next dream cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextDream {
    /// The dream cycle starts tonight.
    Tonight {
        /// The instant at which tonight's dream cycle starts.
        at: Timestamp,
    },
    /// The dream cycle starts tomorrow night.
    Tomorrow {
        /// The instant at which tomorrow night's dream cycle starts.
        at: Timestamp,
    },
    /// The previous dream cycle was missed (machine was off).
    Missed {
        /// The instant at which the dream cycle should have started.
        should_have_run_at: Timestamp,
    },
}

impl Default for DesireClock {
    fn default() -> Self {
        Self {
            reflection_hour: 3,   // 03:00
            reflection_minute: 0, // 00 minutes
        }
    }
}

impl DesireClock {
    /// Creates a Desire Clock with the given time-of-day.
    #[must_use]
    pub fn new(reflection_hour: u32, reflection_minute: u32) -> Self {
        Self {
            reflection_hour,
            reflection_minute,
        }
    }

    /// Computes the time of the next dream cycle relative to `now`.
    ///
    /// Returns:
    /// - [`NextDream::Tonight`] if tonight's reflection time has not yet passed
    /// - [`NextDream::Tomorrow`] if tonight's reflection time has already passed
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
            // Tonight's reflection time has not yet passed → dream cycle tonight
            NextDream::Tonight {
                at: today_reflection,
            }
        } else {
            // Tonight's reflection time has passed → dream cycle tomorrow
            let tomorrow_reflection = today_reflection + chrono::Duration::days(1);
            NextDream::Tomorrow {
                at: tomorrow_reflection,
            }
        }
    }

    /// Computes the most recent dream cycle time relative to `now`.
    ///
    /// Returns:
    /// - [`NextDream::Missed`] if tonight's reflection time has not yet passed (machine was off last night)
    /// - [`NextDream::Tonight`] if tonight's reflection time has passed (machine was on today)
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
            // Tonight's reflection time has not yet passed → the most recent dream cycle was last night
            let yesterday_reflection = today_reflection - chrono::Duration::days(1);
            NextDream::Missed {
                should_have_run_at: yesterday_reflection,
            }
        } else {
            // Tonight's reflection time has passed → the most recent dream cycle was tonight
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
