//! Polttoaineen mittaus (fuel metering) sandboxin suoritukselle.
//!
//! "Fuel" on wasmtimen mekanismi suorituksen kustannuksen rajoittamiseen:
//! jokainen WASM-operaatio kuluttaa polttoainetta, ja kun raja saavutetaan,
//! suoritus keskeytyy. Tämä estää ikuiset silmukat ja resurssien
//! väärinkäytön (design §2 turva). Tässä moduulissa on **puhdas
//! laskentalogiikka** ilman wasmtime-riippuvuutta, jotta budjetointi on
//! testattavaa ilman raskasta backendia.

use serde::{Deserialize, Serialize};

/// Polttoaineraja yhden suorituksen kululle.
///
/// `Limited` antaa tarkan budjetin; `Unlimited` poistaa rajoituksen
/// (käytä vain täysin luotetulle koodille — oletus on aina rajoitettu).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "amount")]
pub enum FuelLimit {
    /// Rajattu budjetti — annettu määrä polttoaineyksiköitä.
    Limited(u64),

    /// Ei rajaa. Vain luotetulle koodille; oletuksena EI käytetä.
    Unlimited,
}

impl FuelLimit {
    /// Konservatiivinen oletusbudjetti (yksi miljoona yksikköä).
    ///
    /// Riittää kevyeen laskentaan mutta katkaisee ikuiset silmukat. Sopivan
    /// arvon viritys on kutsujan vastuulla työkuorman mukaan.
    pub const DEFAULT_BUDGET: u64 = 1_000_000;

    /// Rakentaa rajatun budjetin.
    #[must_use]
    pub const fn limited(amount: u64) -> Self {
        Self::Limited(amount)
    }

    /// Onko budjetti rajaton.
    #[must_use]
    pub const fn is_unlimited(&self) -> bool {
        matches!(self, Self::Unlimited)
    }

    /// Palauttaa rajatun budjetin määrän, tai `None` jos rajaton.
    #[must_use]
    pub const fn budget(&self) -> Option<u64> {
        match self {
            Self::Limited(amount) => Some(*amount),
            Self::Unlimited => None,
        }
    }

    /// Riittääkö budjetti annetulle kulutukselle.
    ///
    /// Rajaton riittää aina. Rajattu riittää jos `consumed <= budget`.
    #[must_use]
    pub const fn covers(&self, consumed: u64) -> bool {
        match self {
            Self::Limited(amount) => consumed <= *amount,
            Self::Unlimited => true,
        }
    }
}

impl Default for FuelLimit {
    /// Turvallinen oletus: rajattu [`FuelLimit::DEFAULT_BUDGET`]-budjetti.
    fn default() -> Self {
        Self::Limited(Self::DEFAULT_BUDGET)
    }
}

/// Polttoainemittari joka seuraa yhden suorituksen kulutusta budjettia vasten.
///
/// Mittari on tilallinen: [`consume`](FuelMeter::consume) vähentää jäljellä
/// olevaa budjettia ja palauttaa virheen jos budjetti loppuu. Tämä mallintaa
/// wasmtimen `add_fuel` / `fuel_consumed` -semantiikkaa testattavasti.
#[derive(Debug, Clone)]
pub struct FuelMeter {
    limit: FuelLimit,
    consumed: u64,
}

impl FuelMeter {
    /// Luo mittarin annetulla rajalla, kulutus nollasta.
    #[must_use]
    pub const fn new(limit: FuelLimit) -> Self {
        Self { limit, consumed: 0 }
    }

    /// Luo mittarin rajatulla budjetilla.
    #[must_use]
    pub const fn with_budget(budget: u64) -> Self {
        Self::new(FuelLimit::Limited(budget))
    }

    /// Tähän mennessä kulutettu polttoaine.
    #[must_use]
    pub const fn consumed(&self) -> u64 {
        self.consumed
    }

    /// Konfiguroitu raja.
    #[must_use]
    pub const fn limit(&self) -> FuelLimit {
        self.limit
    }

    /// Jäljellä oleva budjetti, tai `None` jos rajaton.
    ///
    /// Ei koskaan mene miinukselle: jos budjetti on ylittynyt (mitä `consume`
    /// ei salli tapahtua hiljaa), tämä palauttaa `0`.
    #[must_use]
    pub const fn remaining(&self) -> Option<u64> {
        match self.limit {
            FuelLimit::Limited(budget) => Some(budget.saturating_sub(self.consumed)),
            FuelLimit::Unlimited => None,
        }
    }

    /// Onko polttoaine loppunut (vain rajatulla budjetilla).
    #[must_use]
    pub const fn is_exhausted(&self) -> bool {
        match self.limit {
            FuelLimit::Limited(budget) => self.consumed >= budget,
            FuelLimit::Unlimited => false,
        }
    }

    /// Kuluttaa `amount` yksikköä polttoainetta.
    ///
    /// Onnistuessaan kasvattaa kulutusta ja palauttaa jäljellä olevan
    /// budjetin (`None` jos rajaton). Jos kulutus ylittäisi budjetin, mittari
    /// asetetaan täyteen kulutukseen (budjetti = consumed) ja palautetaan
    /// virhe — tila pysyy johdonmukaisena (ei osittaista kulutusta yli rajan).
    ///
    /// # Errors
    /// [`crate::SandboxError::FuelExhausted`] jos budjetti ei riitä `amount`:lle.
    pub fn consume(&mut self, amount: u64) -> crate::Result<Option<u64>> {
        match self.limit {
            FuelLimit::Limited(budget) => {
                // Lasketaan uusi kulutus ylivuototurvallisesti.
                let next = self.consumed.saturating_add(amount);
                if next > budget {
                    // Pinnataan kulutus budjettiin: mittari on "tyhjä",
                    // ei mielivaltaisesti yli.
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
        // Tila johdonmukainen: kulutus pinnattu budjettiin, ei yli.
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
        // Saturoituu, ei panikoi ylivuodosta.
        assert_eq!(meter.consume(1).expect("still ok"), None);
        assert_eq!(meter.consumed(), u64::MAX);
    }

    #[test]
    fn consume_saturates_on_overflow_for_limited() {
        // Lähellä u64-rajaa: saturating_add ei panikoi, ja koska next > budget,
        // saadaan virhe eikä ylivuotoa.
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
