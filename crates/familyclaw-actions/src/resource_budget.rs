//! Resurssibudjetti ja -vuokra (resource budget / lease): vastapaineen
//! päätöskerros (KERROS A, geneerinen).
//!
//! Tämä moduuli päättää **milloin** tehtävä on keskeytettävä vastapaineen vuoksi
//! (ks. [`crate::task::TaskStatus::Suspended`]). Se EI itse aja tehtäviä eikä
//! lue kelloa — se vain kirjaa kuinka monta samanaikaista suoritusta on käynnissä
//! ja myöntää [`ResourceLease`]-vuokria niin kauan kuin budjetti riittää.
//!
//! ## Malli
//! - **Globaali samanaikaisuuskatto** (`max_concurrent`): koko ajossa olevien
//!   tehtävien yläraja.
//! - **Per-taito-samanaikaisuuskatto** (`per_skill_concurrency`): yhden taidon
//!   ([`SkillId`]) samanaikaisten suoritusten yläraja.
//! - **Jonon pituuskatto** (`max_queue_len`): kutsuja kertoo nykyisen jonon
//!   pituuden; budjetti hylkää uuden varauksen jos jono on jo täynnä
//!   (fail-closed, ei rajatonta jonoa).
//!
//! ## Fail-closed
//! Jos mikä tahansa katto on saavutettu, [`ResourceBudget::try_acquire`] palauttaa
//! [`AcquireOutcome::Unavailable`] **geneerisellä, salaisuudettomalla syyllä** —
//! tämä syy sopii suoraan [`crate::task::TaskQueue::suspend`]:n `reason`-kenttään.
//! Ei panikointia, ei busy-loopia: kutsuja keskeyttää tehtävän ja yrittää
//! myöhemmin uudelleen kun [`ResourceLease`] vapautuu.
//!
//! ## RAII
//! [`ResourceLease`] vapauttaa varatun kapasiteetin automaattisesti kun se
//! pudotetaan ([`Drop`]). Näin laskuri ei voi vuotaa vaikka suoritus panikoisi.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::ids::SkillId;

/// Vastapaine-budjetin yläराjat. Kaikki katot ovat valinnaisia: `None` =
/// kyseistä rajaa ei valvota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetLimits {
    /// Globaali samanaikaisten suoritusten yläraja (`None` = rajaton).
    pub max_concurrent: Option<usize>,
    /// Per-taito samanaikaisten suoritusten yläraja (`None` = rajaton).
    pub per_skill_concurrency: Option<usize>,
    /// Jonon suurin sallittu pituus uutta varausta tehtäessä (`None` = rajaton).
    pub max_queue_len: Option<usize>,
}

impl BudgetLimits {
    /// Rajaton budjetti — ei valvo mitään kattoa (oletus / testaus).
    #[must_use]
    pub const fn unbounded() -> Self {
        Self {
            max_concurrent: None,
            per_skill_concurrency: None,
            max_queue_len: None,
        }
    }
}

impl Default for BudgetLimits {
    fn default() -> Self {
        Self::unbounded()
    }
}

/// [`ResourceBudget::try_acquire`]:n lopputulos: joko myönnetty vuokra tai
/// geneerinen syy keskeytykselle.
#[derive(Debug)]
pub enum AcquireOutcome {
    /// Budjetti riitti: vuokra myönnetty. Kapasiteetti vapautuu kun
    /// [`ResourceLease`] pudotetaan.
    Granted(ResourceLease),
    /// Budjetti ei riittänyt. Sisältää salaisuudettoman syyn joka sopii suoraan
    /// [`crate::task::TaskQueue::suspend`]:n `reason`-argumentiksi.
    Unavailable(String),
}

/// Jaettu, säikeenturvallinen laskuri samanaikaisista suorituksista taidoittain.
#[derive(Debug, Default)]
struct BudgetState {
    /// Käynnissä olevien suoritusten kokonaismäärä.
    total_active: usize,
    /// Taito → sen käynnissä olevien suoritusten määrä.
    per_skill_active: HashMap<SkillId, usize>,
}

/// Vastapaine-budjetti. Klonattava kahva jaettuun tilaan (sisäinen `Arc<Mutex>`),
/// joten useat työntekijät voivat jakaa saman budjetin.
#[derive(Debug, Clone)]
pub struct ResourceBudget {
    limits: BudgetLimits,
    state: Arc<Mutex<BudgetState>>,
}

impl ResourceBudget {
    /// Rakentaa budjetin annetuilla katoilla.
    #[must_use]
    pub fn new(limits: BudgetLimits) -> Self {
        Self {
            limits,
            state: Arc::new(Mutex::new(BudgetState::default())),
        }
    }

    /// Yrittää varata kapasiteetin yhdelle taidon suoritukselle.
    ///
    /// `current_queue_len` on kutsujan ilmoittama nykyisen jonon pituus —
    /// budjetti hylkää varauksen jos se on jo saavuttanut `max_queue_len`:n
    /// (fail-closed, estää rajattoman jonon kasvun).
    ///
    /// Palauttaa [`AcquireOutcome::Granted`] vuokran kanssa jos kaikki katot
    /// sallivat, muutoin [`AcquireOutcome::Unavailable`] geneerisellä syyllä.
    /// Mitään ei kirjata kun varaus hylätään (fail-closed).
    #[must_use]
    pub fn try_acquire(&self, skill_id: SkillId, current_queue_len: usize) -> AcquireOutcome {
        // Jonon pituuskatto tarkistetaan ennen lukon ottamista — ei vaikuta tilaan.
        if let Some(max_q) = self.limits.max_queue_len {
            if current_queue_len >= max_q {
                return AcquireOutcome::Unavailable(format!(
                    "queue_length budget exhausted ({current_queue_len}/{max_q})"
                ));
            }
        }

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(max_c) = self.limits.max_concurrent {
            if state.total_active >= max_c {
                return AcquireOutcome::Unavailable(format!(
                    "max_concurrent budget exhausted ({}/{max_c})",
                    state.total_active
                ));
            }
        }

        if let Some(max_s) = self.limits.per_skill_concurrency {
            let active = state.per_skill_active.get(&skill_id).copied().unwrap_or(0);
            if active >= max_s {
                return AcquireOutcome::Unavailable(format!(
                    "per_skill_concurrency budget exhausted ({active}/{max_s})"
                ));
            }
        }

        // Kaikki katot sallivat: kirjaa varaus.
        state.total_active += 1;
        *state.per_skill_active.entry(skill_id).or_insert(0) += 1;

        AcquireOutcome::Granted(ResourceLease {
            skill_id,
            state: Arc::clone(&self.state),
            released: false,
        })
    }

    /// Käynnissä olevien suoritusten kokonaismäärä (diagnostiikka/testaus).
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .total_active
    }

    /// Yhden taidon käynnissä olevien suoritusten määrä (diagnostiikka/testaus).
    #[must_use]
    pub fn active_for_skill(&self, skill_id: SkillId) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .per_skill_active
            .get(&skill_id)
            .copied()
            .unwrap_or(0)
    }
}

/// Yhden suorituksen varaama kapasiteetti. Vapauttaa varauksen automaattisesti
/// kun se pudotetaan ([`Drop`]) — myös panikoinnin yhteydessä, joten laskuri ei
/// voi vuotaa.
#[derive(Debug)]
pub struct ResourceLease {
    skill_id: SkillId,
    state: Arc<Mutex<BudgetState>>,
    released: bool,
}

impl ResourceLease {
    /// Vapauttaa vuokran eksplisiittisesti (idempotentti: toistettu kutsu /
    /// myöhempi [`Drop`] ei vähennä laskuria uudelleen).
    pub fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.total_active = state.total_active.saturating_sub(1);
        if let Some(active) = state.per_skill_active.get_mut(&self.skill_id) {
            *active = active.saturating_sub(1);
            if *active == 0 {
                state.per_skill_active.remove(&self.skill_id);
            }
        }
    }
}

impl Drop for ResourceLease {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(
        max_c: Option<usize>,
        per_skill: Option<usize>,
        max_q: Option<usize>,
    ) -> BudgetLimits {
        BudgetLimits {
            max_concurrent: max_c,
            per_skill_concurrency: per_skill,
            max_queue_len: max_q,
        }
    }

    #[test]
    fn unbounded_budget_always_grants() {
        let budget = ResourceBudget::new(BudgetLimits::unbounded());
        let skill = SkillId::new();
        for _ in 0..1000 {
            assert!(matches!(
                budget.try_acquire(skill, 0),
                AcquireOutcome::Granted(_)
            ));
        }
    }

    #[test]
    fn max_concurrent_suspends_when_exhausted() {
        let budget = ResourceBudget::new(limits(Some(2), None, None));
        let skill = SkillId::new();
        let _l1 = match budget.try_acquire(skill, 0) {
            AcquireOutcome::Granted(l) => l,
            AcquireOutcome::Unavailable(r) => panic!("unexpected: {r}"),
        };
        let _l2 = match budget.try_acquire(skill, 0) {
            AcquireOutcome::Granted(l) => l,
            AcquireOutcome::Unavailable(r) => panic!("unexpected: {r}"),
        };
        // Kolmas ylittää globaalin katon → keskeytys geneerisellä syyllä.
        match budget.try_acquire(skill, 0) {
            AcquireOutcome::Granted(_) => panic!("should have been unavailable"),
            AcquireOutcome::Unavailable(reason) => {
                assert!(reason.contains("max_concurrent"), "reason: {reason}");
            }
        }
        assert_eq!(budget.active_count(), 2);
    }

    #[test]
    fn lease_release_frees_capacity_for_resume() {
        let budget = ResourceBudget::new(limits(Some(1), None, None));
        let skill = SkillId::new();
        let lease = match budget.try_acquire(skill, 0) {
            AcquireOutcome::Granted(l) => l,
            AcquireOutcome::Unavailable(r) => panic!("unexpected: {r}"),
        };
        assert!(matches!(
            budget.try_acquire(skill, 0),
            AcquireOutcome::Unavailable(_)
        ));
        // Vapauta vuokra → kapasiteetti palautuu → uusi varaus onnistuu (resume).
        drop(lease);
        assert_eq!(budget.active_count(), 0);
        assert!(matches!(
            budget.try_acquire(skill, 0),
            AcquireOutcome::Granted(_)
        ));
    }

    #[test]
    fn per_skill_concurrency_is_independent_per_skill() {
        let budget = ResourceBudget::new(limits(None, Some(1), None));
        let skill_a = SkillId::new();
        let skill_b = SkillId::new();
        let _a = match budget.try_acquire(skill_a, 0) {
            AcquireOutcome::Granted(l) => l,
            AcquireOutcome::Unavailable(r) => panic!("unexpected: {r}"),
        };
        // skill_a on täynnä...
        assert!(matches!(
            budget.try_acquire(skill_a, 0),
            AcquireOutcome::Unavailable(_)
        ));
        // ...mutta skill_b on oma budjettinsa.
        assert!(matches!(
            budget.try_acquire(skill_b, 0),
            AcquireOutcome::Granted(_)
        ));
    }

    #[test]
    fn queue_length_limit_fails_closed_without_recording() {
        let budget = ResourceBudget::new(limits(None, None, Some(10)));
        let skill = SkillId::new();
        // Jono jo täynnä → hylätään KIRJAAMATTA (active pysyy 0).
        match budget.try_acquire(skill, 10) {
            AcquireOutcome::Granted(_) => panic!("should reject full queue"),
            AcquireOutcome::Unavailable(reason) => {
                assert!(reason.contains("queue_length"), "reason: {reason}");
            }
        }
        assert_eq!(budget.active_count(), 0, "hylätty varaus ei saa kirjautua");
    }

    #[test]
    fn unavailable_reason_has_no_secret() {
        // Syy johdetaan vain katoista + laskureista, ei payloadista — siksi se ei
        // voi sisältää salaisuuksia. Tämä testi lukitsee invariantin.
        let budget = ResourceBudget::new(limits(Some(1), None, None));
        let skill = SkillId::new();
        let _l = budget.try_acquire(skill, 0);
        if let AcquireOutcome::Unavailable(reason) = budget.try_acquire(skill, 0) {
            assert!(!reason.contains("sk-"), "reason: {reason}");
            assert!(!reason.to_lowercase().contains("token"), "reason: {reason}");
            assert!(
                !reason.to_lowercase().contains("bearer"),
                "reason: {reason}"
            );
        } else {
            panic!("expected unavailable");
        }
    }

    #[test]
    fn explicit_release_is_idempotent() {
        let budget = ResourceBudget::new(limits(Some(1), None, None));
        let skill = SkillId::new();
        let mut lease = match budget.try_acquire(skill, 0) {
            AcquireOutcome::Granted(l) => l,
            AcquireOutcome::Unavailable(r) => panic!("unexpected: {r}"),
        };
        lease.release();
        lease.release(); // toistettu — ei saa alivuotaa laskuria
        assert_eq!(budget.active_count(), 0);
        drop(lease); // drop release-kutsun jälkeen — ei saa vähentää uudelleen
        assert_eq!(budget.active_count(), 0);
    }
}
