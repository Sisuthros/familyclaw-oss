//! Replay-todistettu **promootiotodiste** (WP2) — KERROS A, OSS.
//!
//! Itseään parantavan agentin klassinen kritiikki: se "luulee aina onnistuneensa".
//! Verdikti perustuu agentin omaan itsearvioon, ei todistettavaan vertailuun.
//! Tämä moduuli vastaa siihen antamalla ehdotukselle **deterministisen,
//! sarjallistuvan todisteen** paremmuudesta: kahden aikajanan (baseline vs.
//! kandidaatti) [`TimeMachine::diff`]-vertailun, jonka pohjalta *kutsujan
//! antama* metriikka päättää parannuksen — ja **todiste itse kirjaa miten
//! verdiktiin päädyttiin**, ei vain totuusarvoa.
//!
//! ## Suhde ehdotuspinoon (suunnittelupäätös)
//!
//! Tämä moduuli on **puhtaasti additiivinen** eikä koske [`crate::Proposal`]-
//! rakenteeseen. Miksei todistetta lisätty kentäksi `Proposal`iin:
//!
//! - `Proposal`in [`content_hash`](crate::Proposal::content_hash) on turvaportti
//!   (TOCTOU-drift): hyväksyntä sitoutuu ehdotuksen tarkkaan sisältöön. Uusi
//!   kenttä muuttaisi kanonista sisältönäkymää ja siten *jokaisen* olemassa
//!   olevan ehdotuksen hajautteen — vanhat katselmoinnit ja sarjallistetut
//!   ehdotukset lakkaisivat täsmäämästä (serde-yhteensopivuusrikko).
//! - Todiste on *liite* ehdotukseen, ei osa sen kuvailevaa sisältöä: sama
//!   ehdotus voi saada uuden todisteen ilman että sen identiteetti tai
//!   katselmoitu sisältö muuttuu.
//!
//! Siksi todiste pidetään **rinnakkaisessa rakenteessa** ([`EvidenceLedger`]),
//! joka avaimena on ehdotuksen [`ProposalId`](crate::ProposalId). Näin ydin
//! pysyy koskemattomana ja rakenteellisesti apply-vapaana.
//!
//! ## Fail-closed
//!
//! [`evaluate_for_approval`] EI koskaan hyväksy mitään — se **vain toteaa**
//! täyttyvätkö todistevaatimukset. Puuttuva todiste, regressio (`improved ==
//! false`) tai tyhjä vertailu → [`EvidenceVerdict::insufficient`]. Ihmisen/
//! operaattorin hyväksyntäporttia ([`crate::ProposalStore::approve`]) tämä ei
//! korvaa vaan täydentää: todistevaatimus on *ehto* hyväksynnälle, ei
//! hyväksyntä itse.

use std::collections::HashMap;

use familyclaw_durable::{Journal, Result as DurableResult, TimeMachine, Timeline, TimelineDiff};
use serde::{Deserialize, Serialize};

use crate::ProposalId;

/// Kutsujan antama parannusmetriikka: **miten** kahden aikajanan diffistä
/// johdetaan verdikti "parani / ei parantunut".
///
/// Metriikka on tarkoituksella eksplisiittinen ja kirjattava, jotta todiste
/// dokumentoi *millä perusteella* paremmuus todettiin — ei pelkkää
/// totuusarvoa. Näin vältetään "agentti luulee onnistuneensa" -ansa: verdiktin
/// peruste on aina luettavissa todisteesta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImprovementMetric {
    /// Parannus = kandidaatti tuotti **enemmän onnistuneita askelia** kuin
    /// baseline (ja vähintään yksi askel verrattiin). Yksinkertainen,
    /// deterministinen oletusmetriikka.
    MoreCompletedSteps,
    /// Parannus = kandidaatti muutti täsmälleen **odotetun määrän** askelia
    /// eikä erkaantunut nimeltään (kohdennettu korjaus, ei villi ajautuminen).
    ExactChangedCount {
        /// Odotettu muuttuneiden askelten määrä.
        expected: usize,
    },
    /// Parannus = aikajanat **eivät erkaantuneet nimeltään** (sama askelrunko)
    /// ja vähintään yksi askel muuttui (jotain oikeasti tapahtui).
    NoDivergenceWithChange,
}

impl ImprovementMetric {
    /// Ajaa metriikan baseline- ja kandidaattiaikajanan yli ja tuottaa
    /// verdiktin sekä ihmisluettavan perustelun.
    ///
    /// Palauttaa `(improved, verdict_reason)`. Perustelu kertoo aina *miksi*
    /// verdikti on se mikä on — myös silloin kun parannusta ei todettu.
    fn evaluate(
        &self,
        baseline: &Timeline,
        candidate: &Timeline,
        diff: &TimelineDiff,
    ) -> (bool, String) {
        match self {
            ImprovementMetric::MoreCompletedSteps => {
                let base_ok = completed_count(baseline);
                let cand_ok = completed_count(candidate);
                let improved = cand_ok > base_ok;
                let reason = format!(
                    "MoreCompletedSteps: candidate completed {cand_ok} step(s) vs baseline \
                     {base_ok} → improved={improved}"
                );
                (improved, reason)
            }
            ImprovementMetric::ExactChangedCount { expected } => {
                let changed = diff.changed_count();
                let diverged = diff.first_divergence();
                let improved = changed == *expected && diverged.is_none();
                let reason = format!(
                    "ExactChangedCount: expected {expected} changed step(s), observed {changed}; \
                     first_divergence={diverged:?} → improved={improved}"
                );
                (improved, reason)
            }
            ImprovementMetric::NoDivergenceWithChange => {
                let diverged = diff.first_divergence();
                let changed = diff.changed_count();
                let improved = diverged.is_none() && changed > 0;
                let reason = format!(
                    "NoDivergenceWithChange: first_divergence={diverged:?}, changed={changed} → \
                     improved={improved}"
                );
                (improved, reason)
            }
        }
    }
}

/// Onnistuneiden (Completed) askelten lukumäärä aikajanalla.
fn completed_count(timeline: &Timeline) -> usize {
    timeline
        .steps
        .iter()
        .filter(|s| s.outcome.is_completed())
        .count()
}

/// Deterministinen, sarjallistuva **todiste** kahden aikajanan vertailusta.
///
/// Kaappaa baseline- ja kandidaattiaikajanan [`TimelineDiff`]-vertailun sekä
/// tiivistelmäkentät ja eksplisiittisen `improved`-verdiktin
/// `verdict_reason`-perusteluineen. Todiste on **inertti data**: se ei sovella
/// eikä hyväksy mitään, se vain *todistaa* millä perusteella kandidaatti oli
/// (tai ei ollut) parannus.
///
/// Rakennetaan [`ReplayEvidence::from_journals`]- tai
/// [`ReplayEvidence::from_timelines`]-kutsulla. Molemmat ovat **lukevia**
/// journalien/aikajanojen suhteen — mikään olemassa oleva aikajana ei muutu.
///
/// ## Miksi diff on `serde_json::Value` eikä `TimelineDiff`
///
/// [`TimelineDiff`] (ja sen `StepDiff`/`StepOutcome`) derivoivat *vain*
/// [`Serialize`]n durable-cratessa, ei [`Deserialize`]a. Jotta todiste on
/// **täysin roundtrippaava** (tallennus + uudelleenluku) koskematta
/// durable-craten tyyppeihin, säilytämme diffin sen **sarjallistetussa
/// muodossa** ([`serde_json::Value`]). Muoto on deterministinen ja
/// ihmisluettava, ja alkuperäinen [`TimelineDiff`] on aina uudelleen
/// johdettavissa ajamalla vertailu uudestaan lähdeaikajanoista.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayEvidence {
    /// Käytetty parannusmetriikka (miten verdiktiin päädyttiin).
    pub metric: ImprovementMetric,
    /// Vertailun tuottanut deterministinen diff **sarjallistettuna**
    /// (baseline vs. kandidaatti). Talletettu [`serde_json::Value`]nä, ks.
    /// tyypin doc miksi.
    pub diff: serde_json::Value,
    /// Verrattujen askelten lukumäärä (diffissä olevat askelparit/-jäännökset).
    pub steps_compared: usize,
    /// Montako askelta muuttui ([`TimelineDiff::changed_count`]).
    pub changed_count: usize,
    /// Montako askelta oli vain toisella aikajanalla
    /// ([`TimelineDiff::tail_count`]).
    pub tail_count: usize,
    /// Ensimmäinen paikka jossa aikajanat erkanivat nimeltään, jos sellainen on.
    pub first_divergence: Option<usize>,
    /// Eksplisiittinen verdikti: oliko kandidaatti parannus.
    pub improved: bool,
    /// Ihmisluettava perustelu verdiktille — *miten* siihen päädyttiin.
    pub verdict_reason: String,
}

impl ReplayEvidence {
    /// Johtaa todisteen kahdesta valmiiksi luetusta aikajanasta annetulla
    /// metriikalla. Puhtaasti laskeva; ei kosketa lähteitä.
    #[must_use]
    pub fn from_timelines(
        baseline: &Timeline,
        candidate: &Timeline,
        metric: ImprovementMetric,
    ) -> Self {
        let diff = TimelineDiff::from_timelines(baseline, candidate);
        let (improved, verdict_reason) = metric.evaluate(baseline, candidate, &diff);
        // Tiivistelmäkentät luetaan aina *oikeasta* TimelineDiffistä, joten
        // verdikti on riippumaton sarjallistuksen onnistumisesta. Vain talletettu
        // diff-arvo on sarjallistettu muoto; käytännössä TimelineDiffin
        // sarjallistus ei epäonnistu (pelkkää dataa), mutta jos silti kävisi,
        // talletetaan `null` (fail-closed: ei paniikkia, tiivistelmät säilyvät).
        let diff_value = serde_json::to_value(&diff).unwrap_or(serde_json::Value::Null);
        Self {
            metric,
            steps_compared: diff.steps.len(),
            changed_count: diff.changed_count(),
            tail_count: diff.tail_count(),
            first_divergence: diff.first_divergence(),
            diff: diff_value,
            improved,
            verdict_reason,
        }
    }

    /// Johtaa todisteen kahdesta journalista annetulla metriikalla: lukee
    /// molemmat aikajanoiksi ([`TimeMachine::diff`]-tyyliin) ja vertaa.
    ///
    /// `baseline` on vertailun lähtökohta ja `candidate` (esim. haarautettu
    /// counterfactual) sen kanssa vertailtava jatko. Kumpaakaan journalia ei
    /// muuteta.
    ///
    /// # Errors
    /// Vie kummankin journalin lukuvirheen läpi
    /// ([`familyclaw_durable::DurableError`]).
    pub fn from_journals<A: Journal, B: Journal>(
        baseline: &A,
        candidate: &B,
        metric: ImprovementMetric,
    ) -> DurableResult<Self> {
        // Sama lukutapa kuin TimeMachine::diff — pidetään yhtenäisenä.
        let base_tl = TimeMachine::inspect(baseline)?;
        let cand_tl = TimeMachine::inspect(candidate)?;
        Ok(Self::from_timelines(&base_tl, &cand_tl, metric))
    }

    /// Onko vertailu **tyhjä** (nollattu askel verrattu) → todiste ei todista
    /// mitään. Fail-closed-arviointi kohtelee tätä riittämättömänä.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps_compared == 0
    }
}

/// Verdikti siitä, täyttääkö ehdotukseen liitetty todiste hyväksynnän
/// **todistevaatimukset**. Tämä EI ole hyväksyntä — se on portin *ehto*.
///
/// Fail-closed: kaikki epävarmuus (puuttuva todiste, regressio, tyhjä
/// vertailu) tuottaa [`EvidenceVerdict::Insufficient`] perusteluineen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceVerdict {
    /// Todistevaatimukset täyttyvät: liitetty todiste on ei-tyhjä ja sen
    /// verdikti on `improved == true`. **Ei silti hyväksyntä** — ihmisen/
    /// operaattorin hyväksyntäportti pysyy polulla.
    RequirementsMet,
    /// Todistevaatimukset EIVÄT täyty → ei hyväksyttävissä (deny-by-default).
    Insufficient {
        /// Ihmisluettava syy miksi todiste ei riitä (auditointijälki).
        reason: String,
    },
}

impl EvidenceVerdict {
    /// Rakentaa riittämättömyys-verdiktin annetulla syyllä.
    fn insufficient(reason: impl Into<String>) -> Self {
        Self::Insufficient {
            reason: reason.into(),
        }
    }

    /// Täyttyivätkö todistevaatimukset.
    #[must_use]
    pub const fn is_met(&self) -> bool {
        matches!(self, EvidenceVerdict::RequirementsMet)
    }
}

/// Arvioi täyttääkö annettu todiste hyväksynnän **todistevaatimukset**.
/// **Fail-closed**: tämä funktio ei hyväksy mitään — se toteaa vain onko
/// hyväksynnän ehto (todistettu paremmuus) olemassa.
///
/// Riittämätön (→ [`EvidenceVerdict::Insufficient`]) kun:
/// - todistetta ei ole liitetty (`None`),
/// - todiste on tyhjä (0 askelta verrattu),
/// - todisteen verdikti on `improved == false` (regressio tai ei parannusta).
///
/// Vain kun todiste on olemassa, ei-tyhjä ja `improved == true`, palautetaan
/// [`EvidenceVerdict::RequirementsMet`]. Silloinkin varsinainen hyväksyntä on
/// erillinen, ihmisen/operaattorin tekemä askel
/// ([`crate::ProposalStore::approve`]) — tämä funktio ei sitä korvaa.
#[must_use]
pub fn evaluate_for_approval(evidence: Option<&ReplayEvidence>) -> EvidenceVerdict {
    let Some(evidence) = evidence else {
        return EvidenceVerdict::insufficient(
            "no replay evidence attached: cannot prove improvement → not approvable \
             (deny-by-default)",
        );
    };
    if evidence.is_empty() {
        return EvidenceVerdict::insufficient(
            "replay evidence compared 0 steps: nothing was proven → not approvable \
             (deny-by-default)",
        );
    }
    if !evidence.improved {
        return EvidenceVerdict::insufficient(format!(
            "replay evidence verdict is improved=false → not approvable (deny-by-default): {}",
            evidence.verdict_reason
        ));
    }
    EvidenceVerdict::RequirementsMet
}

/// Rinnakkainen, additiivinen **todistekirja**: liittää replay-todisteita
/// ehdotuksiin niiden [`ProposalId`]:n kautta koskematta [`crate::Proposal`]-
/// rakenteeseen (eikä siten sen sisältöhajautteeseen).
///
/// Puhtaasti kirjaava/kysyvä — **ei apply-polkua**, samoin kuin
/// [`crate::ProposalStore`]. Todisteen liittäminen ei hyväksy eikä sovella
/// mitään; arviointi tapahtuu [`evaluate_for_approval`]-funktiolla.
#[derive(Debug, Default, Clone)]
pub struct EvidenceLedger {
    evidence: HashMap<ProposalId, ReplayEvidence>,
}

impl EvidenceLedger {
    /// Luo tyhjän todistekirjan.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Liittää (tai korvaa) ehdotukseen replay-todisteen. Palauttaa
    /// mahdollisen aiemman todisteen. EI hyväksy eikä sovella mitään.
    pub fn attach(
        &mut self,
        proposal_id: ProposalId,
        evidence: ReplayEvidence,
    ) -> Option<ReplayEvidence> {
        self.evidence.insert(proposal_id, evidence)
    }

    /// Hakee ehdotukseen liitetyn todisteen, jos sellainen on.
    #[must_use]
    pub fn get(&self, proposal_id: ProposalId) -> Option<&ReplayEvidence> {
        self.evidence.get(&proposal_id)
    }

    /// Arvioi ehdotukseen liitetyn todisteen hyväksynnän todistevaatimuksia
    /// vasten (fail-closed). Puuttuva todiste → [`EvidenceVerdict::Insufficient`].
    #[must_use]
    pub fn evaluate(&self, proposal_id: ProposalId) -> EvidenceVerdict {
        evaluate_for_approval(self.get(proposal_id))
    }

    /// Liitettyjen todisteiden lukumäärä.
    #[must_use]
    pub fn len(&self) -> usize {
        self.evidence.len()
    }

    /// Onko kirja tyhjä.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.evidence.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_durable::{DurableContext, InMemoryJournal};

    /// Apuri: kahden askeleen ajo (load → decide) annetuilla arvoilla.
    fn two_step_run(load: i64, decide: i64) -> InMemoryJournal {
        let mut ctx = DurableContext::new(InMemoryJournal::new()).expect("ctx");
        let a: i64 = ctx.step("load", || Ok(load)).expect("load");
        let _b: i64 = ctx.step("decide", || Ok(a + decide)).expect("decide");
        ctx.finish()
    }

    // ---------- ReplayEvidence-johtaminen ----------

    #[test]
    fn evidence_from_timelines_records_how_verdict_was_reached() {
        // Baseline: yksi epäonnistunut askel. Kandidaatti: sama askel onnistuu.
        let mut base = DurableContext::new(InMemoryJournal::new()).expect("base");
        let _ = base.step::<i32, _>("risky", || Err("boom".to_string()));
        let base = base.finish();

        let mut cand = DurableContext::new(InMemoryJournal::new()).expect("cand");
        let _ = cand.step("risky", || Ok::<_, String>(1)).expect("ok");
        let cand = cand.finish();

        let base_tl = TimeMachine::inspect(&base).expect("base tl");
        let cand_tl = TimeMachine::inspect(&cand).expect("cand tl");
        let ev = ReplayEvidence::from_timelines(
            &base_tl,
            &cand_tl,
            ImprovementMetric::MoreCompletedSteps,
        );

        assert!(ev.improved, "kandidaatti onnistui, baseline ei");
        assert_eq!(ev.steps_compared, 1);
        assert_eq!(ev.changed_count, 1);
        assert!(
            ev.verdict_reason.contains("MoreCompletedSteps"),
            "perustelu kertoo miten verdiktiin päädyttiin: {}",
            ev.verdict_reason
        );
        assert!(!ev.is_empty());
    }

    #[test]
    fn evidence_from_journals_matches_time_machine_diff() {
        let base = two_step_run(10, 5);
        // Kandidaatti: sama load, eri decide → yksi muuttunut askel.
        let cand = two_step_run(10, 7);

        let ev =
            ReplayEvidence::from_journals(&base, &cand, ImprovementMetric::NoDivergenceWithChange)
                .expect("evidence");

        // Sama lukutapa kuin TimeMachine::diff — talletettu diff on sen
        // sarjallistettu muoto.
        let direct = TimeMachine::diff(&base, &cand).expect("diff");
        let direct_value = serde_json::to_value(&direct).expect("serialize diff");
        assert_eq!(ev.diff, direct_value);
        assert_eq!(ev.changed_count, 1);
        assert_eq!(ev.first_divergence, None);
        assert!(ev.improved, "ei erkaantumista + yksi muutos = parannus");
    }

    // ---------- evaluate_for_approval: fail-closed-portti ----------

    #[test]
    fn no_evidence_is_insufficient() {
        let verdict = evaluate_for_approval(None);
        assert!(!verdict.is_met());
        match verdict {
            EvidenceVerdict::Insufficient { reason } => {
                assert!(reason.contains("no replay evidence"));
                assert!(reason.contains("deny-by-default"));
            }
            EvidenceVerdict::RequirementsMet => panic!("missing evidence must be insufficient"),
        }
    }

    #[test]
    fn improvement_meets_requirements() {
        let base = two_step_run(10, 5);
        let cand = two_step_run(10, 9);
        let ev =
            ReplayEvidence::from_journals(&base, &cand, ImprovementMetric::NoDivergenceWithChange)
                .expect("evidence");
        assert!(ev.improved);
        let verdict = evaluate_for_approval(Some(&ev));
        assert_eq!(verdict, EvidenceVerdict::RequirementsMet);
        assert!(verdict.is_met());
    }

    #[test]
    fn regression_is_insufficient() {
        // Identtiset aikajanat → NoDivergenceWithChange: 0 muutosta → ei parannusta.
        let base = two_step_run(10, 5);
        let cand = two_step_run(10, 5);
        let ev =
            ReplayEvidence::from_journals(&base, &cand, ImprovementMetric::NoDivergenceWithChange)
                .expect("evidence");
        assert!(!ev.improved, "ei muutosta → ei parannusta");
        assert!(!ev.is_empty(), "askelia kuitenkin verrattiin");

        let verdict = evaluate_for_approval(Some(&ev));
        assert!(!verdict.is_met());
        match verdict {
            EvidenceVerdict::Insufficient { reason } => {
                assert!(reason.contains("improved=false"));
                assert!(reason.contains("deny-by-default"));
            }
            EvidenceVerdict::RequirementsMet => panic!("regression must be insufficient"),
        }
    }

    #[test]
    fn empty_comparison_is_insufficient() {
        // Kaksi tyhjää journalia → 0 askelta verrattu.
        let base = InMemoryJournal::new();
        let cand = InMemoryJournal::new();
        let ev = ReplayEvidence::from_journals(&base, &cand, ImprovementMetric::MoreCompletedSteps)
            .expect("evidence");
        assert!(ev.is_empty());
        assert_eq!(ev.steps_compared, 0);

        let verdict = evaluate_for_approval(Some(&ev));
        assert!(!verdict.is_met());
        match verdict {
            EvidenceVerdict::Insufficient { reason } => {
                assert!(reason.contains("0 steps"));
            }
            EvidenceVerdict::RequirementsMet => panic!("empty comparison must be insufficient"),
        }
    }

    // ---------- serde-roundtrip ----------

    #[test]
    fn replay_evidence_roundtrips_json() {
        let base = two_step_run(10, 5);
        let cand = two_step_run(10, 8);
        let ev = ReplayEvidence::from_journals(
            &base,
            &cand,
            ImprovementMetric::ExactChangedCount { expected: 1 },
        )
        .expect("evidence");

        let json = serde_json::to_string(&ev).expect("serialize");
        let back: ReplayEvidence = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ev, back);
        // Verdikti + perustelu säilyvät kierroksen yli.
        assert_eq!(ev.improved, back.improved);
        assert_eq!(ev.verdict_reason, back.verdict_reason);
    }

    #[test]
    fn evidence_verdict_roundtrips_json() {
        let met = EvidenceVerdict::RequirementsMet;
        let json = serde_json::to_string(&met).expect("serialize");
        let back: EvidenceVerdict = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(met, back);

        let insufficient = EvidenceVerdict::insufficient("test reason");
        let json = serde_json::to_string(&insufficient).expect("serialize");
        let back: EvidenceVerdict = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(insufficient, back);
    }

    // ---------- EvidenceLedger: rinnakkainen additiivinen rakenne ----------

    #[test]
    fn ledger_attaches_and_evaluates_without_touching_proposal() {
        let base = two_step_run(10, 5);
        let cand = two_step_run(10, 9);
        let ev =
            ReplayEvidence::from_journals(&base, &cand, ImprovementMetric::NoDivergenceWithChange)
                .expect("evidence");

        let mut ledger = EvidenceLedger::new();
        let id = ProposalId::new();

        // Ennen liittämistä: fail-closed (ei todistetta).
        assert!(!ledger.evaluate(id).is_met());
        assert!(ledger.is_empty());

        let prev = ledger.attach(id, ev.clone());
        assert!(prev.is_none());
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger.get(id), Some(&ev));
        assert!(
            ledger.evaluate(id).is_met(),
            "parannus → vaatimukset täyttyvät"
        );
    }

    /// Ydintesti (end-to-end): rakennetaan kaksi oikeaa journalia
    /// [`DurableContext`]illa — baseline ja parannettu counterfactual — ja
    /// johdetaan todiste [`TimeMachine::diff`]-vertailun kautta. Todistaa että
    /// koko ketju (ajo → diff → todiste → fail-closed-arviointi) toimii oikeilla
    /// aikajanoilla.
    #[test]
    fn end_to_end_baseline_vs_improved_counterfactual() {
        // Baseline: kolmivaiheinen ajo jossa "act" epäonnistuu (huono lopputulos).
        let mut base = DurableContext::new(InMemoryJournal::new()).expect("base");
        let amount: i64 = base.step("load", || Ok(100)).expect("load");
        let approved: i64 = base.step("decide", || Ok(amount * 2)).expect("decide");
        let _ = base.step::<String, _>("act", move || {
            let _ = approved;
            Err("baseline act failed".to_string())
        });
        let baseline = base.finish();

        // Haaraudu ennen "act"-askelta ja aja korjattu jatko jossa "act" onnistuu.
        let fork = TimeMachine::fork(&baseline, 2).expect("fork");
        let mut cand = DurableContext::new(fork).expect("cand ctx");
        let amount: i64 = cand.step("load", || Ok(0)).expect("load replay"); // replay lokista
        let approved: i64 = cand.step("decide", || Ok(0)).expect("decide replay"); // replay
        assert_eq!((amount, approved), (100, 200), "prefiksi palautuu lokista");
        let _receipt: String = cand
            .step("act", move || Ok(format!("sent:{approved}")))
            .expect("act candidate");
        let candidate = cand.finish();

        // Johda todiste: kandidaatilla enemmän onnistuneita askelia kuin baselinella.
        let ev = ReplayEvidence::from_journals(
            &baseline,
            &candidate,
            ImprovementMetric::MoreCompletedSteps,
        )
        .expect("evidence");

        assert_eq!(ev.steps_compared, 3);
        assert_eq!(
            ev.first_divergence, None,
            "askelrunko ei erkaantunut nimeltään"
        );
        assert_eq!(ev.changed_count, 1, "vain act muuttui (fail → ok)");
        assert!(ev.improved, "3 onnistunutta vs 2 → parannus");
        assert!(
            ev.verdict_reason.contains("completed 3"),
            "perustelu näyttää mittaustuloksen: {}",
            ev.verdict_reason
        );

        // Fail-closed-portti: todistevaatimukset täyttyvät (mutta EI hyväksyntä).
        let mut ledger = EvidenceLedger::new();
        let id = ProposalId::new();
        ledger.attach(id, ev);
        assert!(ledger.evaluate(id).is_met());

        // Baseline ei muuttunut vertailun aikana (append-only-invariantti).
        let baseline_after = TimeMachine::inspect(&baseline).expect("inspect");
        assert_eq!(baseline_after.len(), 3);
    }
}
