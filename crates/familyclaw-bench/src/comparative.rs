//! [`ComparativeScorecard`]: kahden subjektin tulosten **vertailu kasvotusten**.
//!
//! Tämä on `surpass`-todistuksen julkinen artefakti: sama deterministinen
//! skenaariosarja ajetaan **molemmilla** subjekteilla
//! ([`FamilyClawSubject`](crate::FamilyClawSubject) ja
//! [`MarkdownFileSubject`](crate::subjects::MarkdownFileSubject)) ja tulokset
//! renderöidään rinnakkain kaksisarakkeiseen taulukkoon per skenaario. Lukija
//! näkee yhdellä silmäyksellä missä FamilyClaw läpäisee ja perustaso epäonnistuu.
//!
//! ## Rehellisyysvaroitus (kova vaatimus)
//! Perustaso EI ole aito OpenClaw eikä Hermes Agent — se on
//! *kilpailijan-MUOTOINEN malli* (truncaava `MEMORY.md` + sivuvaikutukset
//! ajetaan uudelleen restartissa). Vertailuraportin **ylätunniste sanoo tämän
//! suoraan**, jottei kukaan voi lukea sitä väitteenä jonkin oikean tuotteen
//! sisäelimistä. Mallinnetut käyttäytymiset ovat dokumentoituja
//! epäonnistumistiloja — ei kärjistyksiä.
//!
//! ## Reprodusoitavuus (design §2.2, §6)
//! Molemmat scorecardit rakennetaan **injektoidulla** kellolla
//! ([`Timestamp`]) — järjestelmäkelloa ei lueta. Skenaariotulokset
//! lajitellaan tunnisteella ([`Scorecard::new`]), ja vertailu yhdistää ne
//! tunnisteen mukaan, joten sama syöte → tavu-tavulta identtinen markdown
//! joka ajolla.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use familyclaw_core::{time, Timestamp};

use crate::scenario::ScenarioResult;
use crate::scorecard::Scorecard;

/// Avain niille mittareille jotka nostetaan vertailutaulukkoon näkyviin
/// (design §3: nämä erottavat durable-replayn truncaavasta perustasosta).
///
/// Jos skenaario ei kirjaa jotakin näistä, sarakkeeseen tulee `—`.
const KEY_METRICS: [&str; 4] = [
    // S1: kuinka moni sivuvaikutus ajettiin uudelleen restartissa (tavoite 0).
    "side_effect_overcount",
    // S1: jatkuiko työ täsmälleen oikeasta askelesta (1.0 = täydellinen).
    "resume_correctness",
    // S2: säilyivätkö identiteetti-ankkurit 90 päivän jälkeen (retention).
    "anchor_retention_90d",
    // S2: löysikö subjektin oma recall odotetut osumat.
    "subject_recall_hits",
];

/// Kahden subjektin scorecardien vertailu kasvotusten.
///
/// `familyclaw` ja `baseline` on ajettu **samalla** skenaariosarjalla ja
/// **samalla** injektoidulla kellolla. [`to_markdown`](Self::to_markdown)
/// renderöi rehellisesti merkityn vertailuraportin.
#[derive(Debug, Clone, PartialEq)]
pub struct ComparativeScorecard {
    /// FamilyClaw-subjektin tuloskortti (ensimmäinen sarake).
    pub familyclaw: Scorecard,
    /// Kilpailijan-muotoisen perustason tuloskortti (toinen sarake).
    pub baseline: Scorecard,
    /// Injektoitu referenssihetki — EI järjestelmäkello (reprodusoitavuus).
    pub clock: Timestamp,
}

/// Yhden mittarin vertailurivi (FamilyClaw vs perustaso) renderöintiä varten.
struct MetricRow {
    /// Mittarin avain (esim. `side_effect_overcount`).
    key: String,
    /// FamilyClaw-arvo muotoiltuna tai `—` jos puuttuu.
    familyclaw: String,
    /// Perustason arvo muotoiltuna tai `—` jos puuttuu.
    baseline: String,
}

impl ComparativeScorecard {
    /// Rakentaa vertailun kahdesta scorecardista ja injektoidusta kellosta.
    ///
    /// Molempien scorecardien skenaariot ovat jo lajiteltuja tunnisteella
    /// ([`Scorecard::new`]:n takaama), joten yhdistäminen tunnisteella on
    /// deterministinen.
    #[must_use]
    pub fn new(familyclaw: Scorecard, baseline: Scorecard, clock: Timestamp) -> Self {
        Self {
            familyclaw,
            baseline,
            clock,
        }
    }

    /// Etsii skenaariotuloksen tunnisteella scorecardista (lineaarinen haku;
    /// skenaarioita on kourallinen).
    fn find<'a>(card: &'a Scorecard, id: &str) -> Option<&'a ScenarioResult> {
        card.scenarios.iter().find(|s| s.id == id)
    }

    /// Muotoilee yhden subjektin pass/fail-merkinnän skenaariolle.
    fn outcome(result: Option<&ScenarioResult>) -> &'static str {
        match result {
            Some(r) if r.passed => "PASS",
            Some(_) => "FAIL",
            None => "—",
        }
    }

    /// Kokoaa skenaarion avainmittarit vertailuriveiksi (deterministinen
    /// järjestys: [`KEY_METRICS`]).
    fn metric_rows(fc: Option<&ScenarioResult>, base: Option<&ScenarioResult>) -> Vec<MetricRow> {
        let fmt = |result: Option<&ScenarioResult>, key: &str| -> String {
            result
                .and_then(|r| r.metrics.get(key))
                .map_or_else(|| "—".to_string(), |v| format!("{v:.4}"))
        };
        KEY_METRICS
            .iter()
            .filter(|key| {
                // Näytä mittari vain jos vähintään toinen subjekti kirjasi sen.
                fc.is_some_and(|r| r.metrics.contains_key(**key))
                    || base.is_some_and(|r| r.metrics.contains_key(**key))
            })
            .map(|key| MetricRow {
                key: (*key).to_string(),
                familyclaw: fmt(fc, key),
                baseline: fmt(base, key),
            })
            .collect()
    }

    /// Onnistuiko FamilyClaw S1 Crash Matrix -skenaariossa siellä missä
    /// perustaso epäonnistuu — eli `side_effect_overcount: 0` vs `> 0`.
    ///
    /// Tämä on `surpass`-todistuksen ydinväite koneluettavana: durable-replay
    /// ajaa sivuvaikutukset täsmälleen kerran, truncaava perustaso ajaa ne
    /// uudelleen. Palauttaa `true` vain jos molemmat subjektit kirjasivat
    /// `side_effect_overcount`-mittarin ja FamilyClaw = 0 < perustaso.
    #[must_use]
    pub fn familyclaw_wins_crash_matrix(&self) -> bool {
        let id = "s1_crash_matrix";
        let fc = Self::find(&self.familyclaw, id);
        let base = Self::find(&self.baseline, id);
        let metric = "side_effect_overcount";
        let (Some(fc), Some(base)) = (fc, base) else {
            return false;
        };
        let (Some(&fc_val), Some(&base_val)) = (fc.metrics.get(metric), base.metrics.get(metric))
        else {
            return false;
        };
        // FamilyClaw ajaa nolla sivuvaikutusta uudelleen; perustaso ajaa > 0,
        // ja FamilyClaw läpäisee skenaarion siinä missä perustaso ei.
        fc_val == 0.0 && base_val > 0.0 && fc.passed && !base.passed
    }

    /// Renderöi vertailun ihmisluettavaksi markdowniksi (`COMPARISON.md`).
    ///
    /// Rakenne:
    /// 1. **Rehellisyys-ylätunniste** — perustaso on kilpailijan-MUOTOINEN
    ///    malli, EI aito OpenClaw/Hermes-instanssi.
    /// 2. **Yhteenvetotaulukko** — kokonaistulos per subjekti.
    /// 3. **Per-skenaario** — kaksisarakkeinen PASS/FAIL + avainmittarit.
    ///
    /// Tuloste on tavu-tavulta deterministinen: kentät kiinteässä
    /// järjestyksessä, skenaariot tunnisteen mukaan, mittarit
    /// [`KEY_METRICS`]-järjestyksessä, kello injektoidusta arvosta.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();

        // 1) Otsikko + rehellisyysvaroitus (kova vaatimus).
        out.push_str("# FamilyClaw vs Baseline — Continuity Comparison\n\n");
        out.push_str(
            "> **Honesty note:** the baseline is a *competitor-SHAPED model* \
             (a `MEMORY.md` that truncates oldest-first + side effects re-run \
             on restart), **NOT** a real OpenClaw / Hermes Agent instance. It \
             models the documented failure modes those file-based memories \
             exhibit — it does not claim to be any real product's internals.\n\n",
        );
        let _ = writeln!(
            out,
            "- **Reference clock (injected):** {}",
            time::to_rfc3339(self.clock)
        );
        let _ = writeln!(out, "- **FamilyClaw subject:** {}", self.familyclaw.subject);
        let _ = writeln!(out, "- **Baseline subject:** {}\n", self.baseline.subject);

        // 2) Yhteenvetotaulukko: kokonaistulos kasvotusten.
        out.push_str("## Summary\n\n");
        out.push_str("| Subject | Overall |\n|---------|---------|\n");
        let _ = writeln!(
            out,
            "| {} (FamilyClaw) | {} |",
            self.familyclaw.subject,
            if self.familyclaw.all_passed() {
                "PASS"
            } else {
                "FAIL"
            }
        );
        let _ = writeln!(
            out,
            "| {} (baseline) | {} |",
            self.baseline.subject,
            if self.baseline.all_passed() {
                "PASS"
            } else {
                "FAIL"
            }
        );
        out.push('\n');

        // 3) Per-skenaario vertailu. Skenaariotunnisteet kerätään molemmista
        //    korteista BTreeSet:iin → deterministinen aakkosjärjestys.
        let ids: BTreeSet<&str> = self
            .familyclaw
            .scenarios
            .iter()
            .chain(self.baseline.scenarios.iter())
            .map(|s| s.id.as_str())
            .collect();

        for id in ids {
            let fc = Self::find(&self.familyclaw, id);
            let base = Self::find(&self.baseline, id);

            let _ = writeln!(out, "## {id}\n");
            out.push_str("| Dimension | FamilyClaw | Baseline |\n");
            out.push_str("|-----------|------------|----------|\n");
            let _ = writeln!(
                out,
                "| result | {} | {} |",
                Self::outcome(fc),
                Self::outcome(base)
            );
            for row in Self::metric_rows(fc, base) {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} |",
                    row.key, row.familyclaw, row.baseline
                );
            }
            out.push('\n');
        }

        // 4) Verdict — koneluettava ydinväite proosana.
        out.push_str("## Verdict\n\n");
        if self.familyclaw_wins_crash_matrix() {
            out.push_str(
                "On **S1 Crash Matrix**, FamilyClaw re-executes \
                 `side_effect_overcount: 0` side effects across every crash point \
                 and passes; the baseline re-runs `> 0` side effects on restart \
                 and fails. Durable replay runs each side effect exactly once — \
                 the truncating file-memory baseline cannot.\n",
            );
        } else {
            out.push_str(
                "S1 Crash Matrix comparison did not establish the expected \
                 FamilyClaw advantage in this run (see the table above).\n",
            );
        }

        out
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Vakiot 0.0/>0.0 ovat tarkkoja float-arvoja testeissä.
mod tests {
    use super::*;
    use crate::scorecard::Scorecard;

    /// Kiinteä injektoitu kello — testeissä vakio (reprodusoitavuus).
    fn fixed_clock() -> Timestamp {
        time::from_unix_secs(1_717_000_000).expect("valid clock")
    }

    /// Rakentaa FamilyClaw-tyylisen "voittaja"-scorecardin: S1 läpäisee
    /// nollalla sivuvaikutusten ylityksellä.
    fn familyclaw_card() -> Scorecard {
        let s1 = ScenarioResult::new("s1_crash_matrix", true)
            .with_metric("resume_correctness", 1.0)
            .with_metric("side_effect_overcount", 0.0)
            .with_metric("result_matches_baseline", 1.0);
        let s2 = ScenarioResult::new("s2_retention_curve", true)
            .with_metric("anchor_retention_90d", 1.0)
            .with_metric("subject_recall_hits", 4.0);
        Scorecard::new("familyclaw", vec![s1, s2], fixed_clock())
    }

    /// Rakentaa perustason "häviäjä"-scorecardin: S1 epäonnistuu koska
    /// sivuvaikutuksia ajetaan uudelleen.
    fn baseline_card() -> Scorecard {
        let s1 = ScenarioResult::new("s1_crash_matrix", false)
            .with_metric("resume_correctness", 0.0)
            .with_metric("side_effect_overcount", 12.0)
            .with_metric("result_matches_baseline", 0.0);
        let s2 = ScenarioResult::new("s2_retention_curve", false)
            .with_metric("anchor_retention_90d", 0.0)
            .with_metric("subject_recall_hits", 0.0);
        Scorecard::new("markdown-file-baseline", vec![s1, s2], fixed_clock())
    }

    #[test]
    fn markdown_is_byte_for_byte_reproducible() {
        let cmp_a = ComparativeScorecard::new(familyclaw_card(), baseline_card(), fixed_clock());
        let cmp_b = ComparativeScorecard::new(familyclaw_card(), baseline_card(), fixed_clock());
        assert_eq!(
            cmp_a.to_markdown(),
            cmp_b.to_markdown(),
            "sama syöte → tavu-tavulta identtinen vertailuraportti"
        );
    }

    #[test]
    fn report_has_honesty_header() {
        let cmp = ComparativeScorecard::new(familyclaw_card(), baseline_card(), fixed_clock());
        let md = cmp.to_markdown();
        assert!(
            md.contains("competitor-SHAPED model"),
            "ylätunniste merkitsee perustason rehellisesti"
        );
        assert!(
            md.contains("NOT") && md.contains("OpenClaw") && md.contains("Hermes"),
            "raportti kieltää eksplisiittisesti olevansa aito tuote"
        );
    }

    #[test]
    fn familyclaw_passes_crash_matrix_where_baseline_fails() {
        let cmp = ComparativeScorecard::new(familyclaw_card(), baseline_card(), fixed_clock());
        assert!(
            cmp.familyclaw_wins_crash_matrix(),
            "FamilyClaw läpäisee S1 (side_effect_overcount=0) siellä missä \
             perustaso epäonnistuu (side_effect_overcount>0)"
        );

        // Ja sama näkyy renderöidyssä raportissa.
        let md = cmp.to_markdown();
        // S1-rivillä FamilyClaw=0.0000, baseline=12.0000.
        assert!(md.contains("| side_effect_overcount | 0.0000 | 12.0000 |"));
        // Verdict toteaa edun.
        assert!(md.contains("runs each side effect exactly once"));
    }

    #[test]
    fn wins_is_false_when_baseline_also_zero() {
        // Jos perustaso EI aja sivuvaikutuksia uudelleen, väite ei päde.
        let weak_baseline = Scorecard::new(
            "markdown-file-baseline",
            vec![ScenarioResult::new("s1_crash_matrix", true)
                .with_metric("side_effect_overcount", 0.0)],
            fixed_clock(),
        );
        let cmp = ComparativeScorecard::new(familyclaw_card(), weak_baseline, fixed_clock());
        assert!(
            !cmp.familyclaw_wins_crash_matrix(),
            "ilman perustason sivuvaikutus-ylitystä edusväite ei päde"
        );
    }

    #[test]
    fn missing_metric_renders_as_dash() {
        // Skenaario ilman avainmittareita → sarakkeissa lukee '—' tai jätetään pois.
        let bare_fc = Scorecard::new(
            "familyclaw",
            vec![ScenarioResult::new("s9_bare", true)],
            fixed_clock(),
        );
        let bare_base = Scorecard::new(
            "markdown-file-baseline",
            vec![ScenarioResult::new("s9_bare", false)],
            fixed_clock(),
        );
        let cmp = ComparativeScorecard::new(bare_fc, bare_base, fixed_clock());
        let md = cmp.to_markdown();
        // Tuloksen pass/fail näkyy aina, vaikka mittareita ei olisi.
        assert!(md.contains("| result | PASS | FAIL |"));
    }
}
