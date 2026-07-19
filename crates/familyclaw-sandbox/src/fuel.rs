//! Fuel metering for sandbox execution.
//!
//! "Fuel" is wasmtime's mechanism for bounding execution cost: every WASM
//! operation consumes fuel, and once the limit is reached, execution is
//! interrupted. This prevents infinite loops and resource abuse (design §2
//! security). This module contains **pure accounting logic** with no
//! wasmtime dependency, so budgeting is testable without the heavy backend.

use serde::{Deserialize, Serialize};

/// The fuel limit for a single execution's cost.
///
/// `Limited` gives an exact budget; `Unlimited` removes the restriction
/// (use only for fully trusted code — the default is always limited).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "amount")]
pub enum FuelLimit {
    /// A limited budget — a given number of fuel units.
    Limited(u64),

    /// No limit. For trusted code only; NOT used by default.
    Unlimited,
}

impl FuelLimit {
    /// A conservative default budget (one million units).
    ///
    /// Enough for light computation but cuts off infinite loops. Tuning an
    /// appropriate value for the workload is the caller's responsibility.
    pub const DEFAULT_BUDGET: u64 = 1_000_000;

    /// Builds a limited budget.
    #[must_use]
    pub const fn limited(amount: u64) -> Self {
        Self::Limited(amount)
    }

    /// Whether the budget is unlimited.
    #[must_use]
    pub const fn is_unlimited(&self) -> bool {
        matches!(self, Self::Unlimited)
    }

    /// Returns the limited budget amount, or `None` if unlimited.
    #[must_use]
    pub const fn budget(&self) -> Option<u64> {
        match self {
            Self::Limited(amount) => Some(*amount),
            Self::Unlimited => None,
        }
    }

    /// Whether the budget covers the given consumption.
    ///
    /// Unlimited always covers it. Limited covers it if `consumed <= budget`.
    #[must_use]
    pub const fn covers(&self, consumed: u64) -> bool {
        match self {
            Self::Limited(amount) => consumed <= *amount,
            Self::Unlimited => true,
        }
    }
}

impl Default for FuelLimit {
    /// Safe default: a limited [`FuelLimit::DEFAULT_BUDGET`] budget.
    fn default() -> Self {
        Self::Limited(Self::DEFAULT_BUDGET)
    }
}

/// A fuel meter that tracks a single execution's consumption against a budget.
///
/// The meter is stateful: [`consume`](FuelMeter::consume) decrements the
/// remaining budget and returns an error if the budget runs out. This models
/// wasmtime's `add_fuel` / `fuel_consumed` semantics in a testable way.
#[derive(Debug, Clone)]
pub struct FuelMeter {
    limit: FuelLimit,
    consumed: u64,
}

impl FuelMeter {
    /// Creates a meter with the given limit, consumption starting at zero.
    #[must_use]
    pub const fn new(limit: FuelLimit) -> Self {
        Self { limit, consumed: 0 }
    }

    /// Creates a meter with a limited budget.
    #[must_use]
    pub const fn with_budget(budget: u64) -> Self {
        Self::new(FuelLimit::Limited(budget))
    }

    /// Fuel consumed so far.
    #[must_use]
    pub const fn consumed(&self) -> u64 {
        self.consumed
    }

    /// The configured limit.
    #[must_use]
    pub const fn limit(&self) -> FuelLimit {
        self.limit
    }

    /// Remaining budget, or `None` if unlimited.
    ///
    /// Never goes negative: if the budget has been exceeded (which `consume`
    /// never allows to happen silently), this returns `0`.
    #[must_use]
    pub const fn remaining(&self) -> Option<u64> {
        match self.limit {
            FuelLimit::Limited(budget) => Some(budget.saturating_sub(self.consumed)),
            FuelLimit::Unlimited => None,
        }
    }

    /// Whether the fuel has run out (only meaningful for a limited budget).
    #[must_use]
    pub const fn is_exhausted(&self) -> bool {
        match self.limit {
            FuelLimit::Limited(budget) => self.consumed >= budget,
            FuelLimit::Unlimited => false,
        }
    }

    /// Consumes `amount` units of fuel.
    ///
    /// On success, increases consumption and returns the remaining budget
    /// (`None` if unlimited). If the consumption would exceed the budget,
    /// the meter is set to full consumption (budget = consumed) and an error
    /// is returned — state stays consistent (no partial consumption beyond
    /// the limit).
    ///
    /// # Errors
    /// [`crate::SandboxError::FuelExhausted`] if the budget cannot cover `amount`.
    pub fn consume(&mut self, amount: u64) -> crate::Result<Option<u64>> {
        match self.limit {
            FuelLimit::Limited(budget) => {
                // Compute the new consumption in an overflow-safe way.
                let next = self.consumed.saturating_add(amount);
                if next > budget {
                    // Pin consumption to the budget: the meter is "empty",
                    // not arbitrarily over.
                    self.consumed = budget;
                    return Err(crate::SandboxError::fuel_exhausted(budget, next));
                }
                self.consumed = next;
                Ok(Some(budget - next))
            }
            FuelLimit::Unlimited => {
                self.consumed = self.consumed.saturating_add(amount);
                Ok(None)
            }
        }
    }
}

impl Default for FuelMeter {
    fn default() -> Self {
        Self::new(FuelLimit::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limit_is_limited_to_default_budget() {
        let limit = FuelLimit::default();
        assert_eq!(limit, FuelLimit::Limited(FuelLimit::DEFAULT_BUDGET));
        assert!(!limit.is_unlimited());
        assert_eq!(limit.budget(), Some(FuelLimit::DEFAULT_BUDGET));
    }

    #[test]
    fn unlimited_has_no_budget_and_covers_everything() {
        let limit = FuelLimit::Unlimited;
        assert!(limit.is_unlimited());
        assert_eq!(limit.budget(), None);
        assert!(limit.covers(0));
        assert!(limit.covers(u64::MAX));
    }

    #[test]
    fn limited_covers_only_within_budget() {
        let limit = FuelLimit::limited(100);
        assert!(limit.covers(0));
        assert!(limit.covers(100));
        assert!(!limit.covers(101));
    }

    #[test]
    fn meter_starts_empty() {
        let meter = FuelMeter::with_budget(500);
        assert_eq!(meter.consumed(), 0);
        assert_eq!(meter.remaining(), Some(500));
        assert!(!meter.is_exhausted());
        assert_eq!(meter.limit(), FuelLimit::Limited(500));
    }

    #[test]
    fn consume_decrements_remaining() {
        let mut meter = FuelMeter::with_budget(100);
        let left = meter.consume(30).expect("within budget");
        assert_eq!(left, Some(70));
        assert_eq!(meter.consumed(), 30);
        assert_eq!(meter.remaining(), Some(70));
        assert!(!meter.is_exhausted());
    }

    #[test]
    fn consume_exact_budget_exhausts_meter() {
        let mut meter = FuelMeter::with_budget(100);
        let left = meter.consume(100).expect("exact budget ok");
        assert_eq!(left, Some(0));
        assert!(meter.is_exhausted());
        assert_eq!(meter.remaining(), Some(0));
    }

    #[test]
    fn consume_over_budget_errors_and_pins_to_budget() {
        let mut meter = FuelMeter::with_budget(100);
        let err = meter.consume(150).expect_err("over budget must fail");
        // State is consistent: consumption pinned to the budget, not over.
        assert_eq!(meter.consumed(), 100);
        assert!(meter.is_exhausted());
        assert_eq!(meter.remaining(), Some(0));
        assert!(err.to_string().contains("fuel exhausted"));
    }

    #[test]
    fn consume_after_exhaustion_keeps_failing() {
        let mut meter = FuelMeter::with_budget(10);
        meter.consume(10).expect("exact ok");
        assert!(meter.consume(1).is_err());
        assert_eq!(meter.consumed(), 10);
    }

    #[test]
    fn incremental_consume_until_exhausted() {
        let mut meter = FuelMeter::with_budget(10);
        assert_eq!(meter.consume(4).expect("ok"), Some(6));
        assert_eq!(meter.consume(4).expect("ok"), Some(2));
        assert!(meter.consume(4).is_err());
        assert_eq!(meter.consumed(), 10);
    }

    #[test]
    fn unlimited_meter_never_exhausts() {
        let mut meter = FuelMeter::new(FuelLimit::Unlimited);
        assert_eq!(meter.consume(u64::MAX).expect("unlimited ok"), None);
        assert!(!meter.is_exhausted());
        assert_eq!(meter.remaining(), None);
        // Saturates, does not panic on overflow.
        assert_eq!(meter.consume(1).expect("still ok"), None);
        assert_eq!(meter.consumed(), u64::MAX);
    }

    #[test]
    fn consume_saturates_on_overflow_for_limited() {
        // Near the u64 limit: saturating_add does not panic, and since
        // next > budget, we get an error instead of an overflow.
        let mut meter = FuelMeter::with_budget(10);
        meter.consume(5).expect("ok");
        let err = meter.consume(u64::MAX).expect_err("huge consume fails");
        assert!(err.to_string().contains("fuel exhausted"));
        assert_eq!(meter.consumed(), 10);
    }

    #[test]
    fn default_meter_uses_default_budget() {
        let meter = FuelMeter::default();
        assert_eq!(meter.remaining(), Some(FuelLimit::DEFAULT_BUDGET));
    }

    #[test]
    fn fuel_limit_serde_roundtrip() {
        for limit in [FuelLimit::Limited(42), FuelLimit::Unlimited] {
            let json = serde_json::to_string(&limit).expect("serialize");
            let back: FuelLimit = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(limit, back);
        }
    }
}
