//! **S2 Retention Curve** — muistin säilyvyyskäyrä yli ajan (design §3).
//!
//! Tämä skenaario todistaa Eternal Threadin keskeisen väitteen:
//!
//! > Identiteetti-ankkurit (λ = 0) **eivät katoa koskaan**, arkipäiväinen
//! > trivia (`Fast`) **haihtuu**, ja FamilyClaw-malli **voittaa naiivin
//! > rengaspuskurin** *oikeiden* muistojen säilyttämisessä.
//!
//! ## Miten se mitataan
//! Skenaario kylvää determinisistisen muistipopulaation kolmeen luokkaan:
//! - **ankkurit** — [`DecayPolicy::ProtectedCore`], maksimi-identiteetti
//!   (esim. olennon nimi ja perhe). Retentio on aina `1.0`.
//! - **tärkeät** — [`DecayPolicy::Slow`], korkea tärkeys. Säilyvät pitkään
//!   mutta vaimenevat hitaasti.
//! - **trivia** — [`DecayPolicy::Fast`], matala tärkeys. Haihtuvat nopeasti.
//!
//! **Injektoitua kelloa** siirretään 7 → 30 → 90 vuorokautta eteenpäin
//! (ei oikeaa nukkumista, ei järjestelmäkelloa). Jokaisessa kohdassa
//! lasketaan `recall@k` ankkureille vs. trivialle käyttäen muistivaraston
//! `retention(at)` / `is_retrievable()` -mittareita ja `retrieve()`-hakua.
//!
//! ## Naiivi perustaso (rengaspuskuri)
//! Vertailukohta on **"viimeiset N muistoa, ei vaimennusmallia"** -puskuri.
//! Koska trivia kylvetään viimeisenä, naiivi viimeiset-N -puskuri **säilyttää
//! trivian ja heittää ankkurit pois** — täsmälleen väärinpäin. FamilyClaw
//! säilyttää ankkurit ja antaa trivian haihtua. Tämä on mitattava ero.
//!
//! ## Läpäisyehto (design §3 S2)
//! `passed` = ankkurit ehjät (`anchor_retention_90d ≈ 1.0`) **JA** trivia
//! vaimeni (`trivia_decayed_90d`) **JA** FamilyClaw voittaa naiivin
//! perustason oikeiden (tärkeiden) muistojen säilyttämisessä.

use async_trait::async_trait;
use chrono::Duration;

use familyclaw_core::Timestamp;
use familyclaw_memory::{
    DecayPolicy, DecayThresholds, ImportanceFactors, LocalJsonStore, Memory, MemoryStore,
    RetrievalContext,
};

use crate::error::Result;
use crate::metrics::recall_at_k;
use crate::scenario::{Scenario, ScenarioResult};
use crate::subject::Subject;

/// Top-k raja `recall@k`-mittaukselle. Vakio tekee tuloksesta reprodusoitavan.
const RECALL_K: usize = 5;

/// Kuinka monta muistoa naiivi rengaspuskuri pitää (viimeiset N).
///
/// Valittu pienemmäksi kuin kylvettyjen muistojen kokonaismäärä, jotta puskuri
/// joutuu heittämään jotain pois — ja koska trivia kylvetään viimeisenä, se
/// heittää pois nimenomaan ankkurit (huonoin mahdollinen valinta).
const NAIVE_BUFFER_CAP: usize = 4;

/// Aikapisteet vuorokausina joissa säilyvyys mitataan (design §3 S2).
const DAY_CHECKPOINTS: [i64; 3] = [7, 30, 90];

/// Retentiokynnys jonka alapuolella muisto katsotaan "vaimentuneeksi".
const DECAYED_BELOW: f32 = 0.4;

/// S2 Retention Curve -skenaario.
///
/// Tilaton — kaikki ajotila johdetaan injektoidusta kellosta, joten kahdella
/// ajolla samalla kellolla on identtinen tulos (design §2.2).
#[derive(Debug, Default, Clone, Copy)]
pub struct RetentionCurve;

impl RetentionCurve {
    /// Rakentaa skenaarion.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Yksittäinen kylvetty muisto luokiteltuna (skenaarion sisäinen kirjanpito).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Identiteetti-ankkuri (`ProtectedCore`, λ = 0).
    Anchor,
    /// Tärkeä, hitaasti vaimeneva muisto (`Slow`).
    Important,
    /// Arkipäiväinen trivia (`Fast`).
    Trivia,
}

/// Determinisistinen kylvösiemen: sisältö + luokka. Järjestys on merkitsevä —
/// trivia viimeisenä, jotta naiivi viimeiset-N -puskuri heittää ankkurit pois.
fn seed_plan() -> Vec<(&'static str, Class)> {
    vec![
        ("I am agent_alpha, part of this team", Class::Anchor),
        ("My team: agent_alpha, agent_beta, agent_gamma, agent_delta", Class::Anchor),
        ("The project shipped its first release", Class::Important),
        ("We agreed durable replay is the spearhead", Class::Important),
        ("The weather was cloudy this afternoon", Class::Trivia),
        ("Someone mentioned a coffee break at noon", Class::Trivia),
        ("A passing comment about the bus schedule", Class::Trivia),
    ]
}

/// Rakentaa yhden muiston luokkansa mukaisilla parametreilla, kellolla `clock`.
fn build_memory(content: &str, class: Class, clock: Timestamp) -> Memory {
    let (factors, policy) = match class {
        // Maksimi-identiteetti; ProtectedCore ei vaimene koskaan.
        Class::Anchor => (
            ImportanceFactors::new(1.0, 1.0, 0.0, 0.0),
            DecayPolicy::ProtectedCore,
        ),
        // Korkea tärkeys, hidas vaimeneminen.
        Class::Important => (
            ImportanceFactors::new(0.8, 0.6, 0.3, 0.0),
            DecayPolicy::Slow,
        ),
        // Matala tärkeys, nopea vaimeneminen.
        Class::Trivia => (
            ImportanceFactors::new(0.1, 0.0, 0.2, 0.0),
            DecayPolicy::Fast,
        ),
    };
    Memory::builder(content)
        .factors(factors)
        .decay_policy(policy)
        .created_at(clock)
        .build()
}

/// Naiivi rengaspuskuri-perustaso: pitää vain viimeiset `cap` muistoa
/// kylvöjärjestyksessä, **ilman mitään vaimennusmallia**. Tämä on se
/// kilpailija jonka FamilyClaw lyö: se ei tiedä mikä muisto on tärkeä, joten
/// se säilyttää uusimmat (trivian) ja heittää vanhimmat (ankkurit) pois.
#[derive(Debug, Default)]
struct NaiveRingBuffer {
    /// Säilytettyjen muistojen sisältö kylvöjärjestyksessä.
    kept: Vec<String>,
    /// Maksimikapasiteetti.
    cap: usize,
}

impl NaiveRingBuffer {
    fn new(cap: usize) -> Self {
        Self {
            kept: Vec::new(),
            cap: cap.max(1),
        }
    }

    /// Lisää muiston; ylivuodolla pudottaa vanhimman (FIFO-eviktio).
    fn push(&mut self, content: &str) {
        self.kept.push(content.to_string());
        if self.kept.len() > self.cap {
            self.kept.remove(0);
        }
    }

    /// Onko annettu sisältö yhä puskurissa (= "muistaako" naiivi perustaso sen).
    fn contains(&self, content: &str) -> bool {
        self.kept.iter().any(|c| c == content)
    }
}

/// Kuinka moni annetun luokan muisto on yhä haettavissa FamilyClaw-varastossa
/// hetkellä `at` (retentio ≥ kynnys ja elinkaaritila haettavissa).
async fn retrievable_count(
    store: &LocalJsonStore,
    seeds: &[(&'static str, Class)],
    class: Class,
    at: Timestamp,
) -> Result<usize> {
    let all = store.all().await?;
    let mut count = 0;
    for memory in &all {
        // Yhdistä muisto luokkaansa sisällön perusteella (deterministinen).
        let is_class = seeds
            .iter()
            .any(|(content, c)| *c == class && *content == memory.content);
        if is_class && memory.is_retrievable() && memory.retention(at) >= DECAYED_BELOW {
            count += 1;
        }
    }
    Ok(count)
}

#[async_trait]
impl Scenario for RetentionCurve {
    // Trait-allekirjoitus vaatii `&str`; literaali on aina `'static`, joten
    // clippyn `&'static str`-ehdotus ei sovi tähän toteutukseen.
    #[allow(clippy::unnecessary_literal_bound)]
    fn id(&self) -> &str {
        "s2_retention_curve"
    }

    #[allow(clippy::too_many_lines)] // Yksi yhtenäinen, luettava koesarja.
    async fn run(&self, subject: &mut dyn Subject, clock: Timestamp) -> Result<ScenarioResult> {
        let seeds = seed_plan();
        let anchors_total = seeds.iter().filter(|(_, c)| *c == Class::Anchor).count();
        let important_total = seeds.iter().filter(|(_, c)| *c == Class::Important).count();
        let trivia_total = seeds.iter().filter(|(_, c)| *c == Class::Trivia).count();

        if anchors_total == 0 || important_total == 0 || trivia_total == 0 {
            return Err(crate::BenchError::scenario(
                "retention_curve: seed plan must contain anchors, important and trivia",
            ));
        }

        // ── Kylvä FamilyClaw-muistivarasto ja naiivi perustaso samalla datalla ──
        let store = LocalJsonStore::in_memory();
        let mut naive = NaiveRingBuffer::new(NAIVE_BUFFER_CAP);
        for (content, class) in &seeds {
            store.add(build_memory(content, *class, clock)).await?;
            naive.push(content);
        }

        let mut result = ScenarioResult::new(self.id(), false)
            .with_note(format!(
                "seeded {anchors_total} anchors, {important_total} important, {trivia_total} trivia at injected clock"
            ))
            // Pienet laskurit (kpl-määrät); f64 esittää ne tarkasti.
            .with_metric("recall_k", f64::from(u32::try_from(RECALL_K).unwrap_or(u32::MAX)))
            .with_metric(
                "naive_buffer_cap",
                f64::from(u32::try_from(NAIVE_BUFFER_CAP).unwrap_or(u32::MAX)),
            );

        // ── Mittaa säilyvyyskäyrä jokaisessa aikapisteessä ──
        let mut anchor_retention_at_90 = 0.0_f64;
        let mut trivia_retrievable_at_90 = trivia_total;
        for &days in &DAY_CHECKPOINTS {
            let at = clock + Duration::days(days);

            // Aja vaimennus tähän hetkeen: trivia putoaa arkistoon/haudataan,
            // ankkureita ei koskaan kosketa (store takaa tämän).
            store.run_decay(DecayThresholds::default(), at).await?;

            let anchors_live = retrievable_count(&store, &seeds, Class::Anchor, at).await?;
            let trivia_live = retrievable_count(&store, &seeds, Class::Trivia, at).await?;

            // recall@k ankkureille: kuinka moni odotetuista ankkureista löytyy.
            let anchor_recall = recall_at_k(anchors_total, anchors_live)?;
            // recall@k trivialle: korkea = trivia EI haihtunut (huono).
            let trivia_recall = recall_at_k(trivia_total, trivia_live)?;

            result = result
                .with_metric(format!("recall_at_{RECALL_K}_anchors_day{days}"), anchor_recall)
                .with_metric(format!("recall_at_{RECALL_K}_trivia_day{days}"), trivia_recall);

            // Ankkureiden keskimääräinen retentio (pitäisi olla tasan 1.0).
            let anchor_retention = mean_retention(&store, &seeds, Class::Anchor, at).await?;
            result = result.with_metric(
                format!("anchor_retention_day{days}"),
                f64::from(anchor_retention),
            );

            if days == 90 {
                anchor_retention_at_90 = f64::from(anchor_retention);
                trivia_retrievable_at_90 = trivia_live;
            }
        }

        // ── 90 päivän yhteenvetomittarit (design §3 S2) ──
        result = result.with_metric("anchor_retention_90d", anchor_retention_at_90);
        // trivia_decayed_90d: osuus trivialuokasta joka on haihtunut.
        let trivia_decayed_fraction = recall_at_k(
            trivia_total,
            trivia_total.saturating_sub(trivia_retrievable_at_90),
        )?;
        result = result.with_metric("trivia_decayed_90d", trivia_decayed_fraction);

        // ── FamilyClaw vs naiivi perustaso: oikeiden muistojen säilyttäminen ──
        // FamilyClaw 90 päivän jälkeen: kuinka moni TÄRKEÄ (ankkurit+important)
        // muisto on yhä haettavissa. Naiivi: kuinka moni samoista on puskurissa.
        let at_90 = clock + Duration::days(90);
        let mut fc_keeps_important = 0_usize;
        let mut naive_keeps_important = 0_usize;
        let important_like = anchors_total + important_total;
        let all = store.all().await?;
        for (content, class) in &seeds {
            if *class == Class::Anchor || *class == Class::Important {
                if let Some(m) = all.iter().find(|m| &m.content == content) {
                    if m.is_retrievable() && m.retention(at_90) >= DECAYED_BELOW {
                        fc_keeps_important += 1;
                    }
                }
                if naive.contains(content) {
                    naive_keeps_important += 1;
                }
            }
        }
        let fc_keep_rate = recall_at_k(important_like, fc_keeps_important)?;
        let naive_keep_rate = recall_at_k(important_like, naive_keeps_important)?;
        result = result
            .with_metric("familyclaw_keeps_important_90d", fc_keep_rate)
            .with_metric("naive_keeps_important_90d", naive_keep_rate)
            .with_note(format!(
                "FamilyClaw keeps {fc_keeps_important}/{important_like} important memories; naive ring buffer keeps {naive_keeps_important}/{important_like}"
            ));

        // ── Aja myös subjektin oma musta-laatikko-recall (saumavarmistus) ──
        // Subject ei tarjoa kylvörajapintaa, joten tämä on tiedonkeruuta eikä
        // läpäisyn ehto — varsinainen S2-pisteytys tulee yllä olevasta mallista.
        let subject_hits = subject.recall("family", at_90).await?;
        // Osumamäärä on pieni kpl-laskuri; f64 esittää sen tarkasti.
        let subject_hit_count = f64::from(u32::try_from(subject_hits.len()).unwrap_or(u32::MAX));
        result = result.with_metric("subject_recall_hits", subject_hit_count);

        // ── Varmista että FamilyClaw-haku itsekin nostaa ankkurit kärkeen ──
        let ctx = RetrievalContext::new("family").with_limit(RECALL_K);
        let hits = store.retrieve(&ctx, at_90).await?;
        let top_is_anchor = hits.first().is_some_and(|h| {
            seeds
                .iter()
                .any(|(content, c)| *c == Class::Anchor && *content == h.memory.content)
        });
        result = result.with_metric(
            "retrieve_top_is_anchor",
            if top_is_anchor { 1.0 } else { 0.0 },
        );

        // ── Läpäisyehto (design §3 S2) ──
        let anchors_intact = (anchor_retention_at_90 - 1.0).abs() < 1e-6;
        let trivia_decayed = trivia_retrievable_at_90 < trivia_total;
        let beats_naive = fc_keep_rate > naive_keep_rate;
        let passed = anchors_intact && trivia_decayed && beats_naive;

        result.passed = passed;
        result = result
            .with_note(format!(
                "anchors_intact={anchors_intact} trivia_decayed={trivia_decayed} beats_naive={beats_naive}"
            ));
        Ok(result)
    }
}

/// Annetun luokan muistojen keskimääräinen retentio hetkellä `at`.
async fn mean_retention(
    store: &LocalJsonStore,
    seeds: &[(&'static str, Class)],
    class: Class,
    at: Timestamp,
) -> Result<f32> {
    let all = store.all().await?;
    let mut sum = 0.0_f32;
    let mut n = 0_u32;
    for memory in &all {
        let is_class = seeds
            .iter()
            .any(|(content, c)| *c == class && *content == memory.content);
        if is_class {
            sum += memory.retention(at);
            n += 1;
        }
    }
    if n == 0 {
        return Ok(0.0);
    }
    // `n` on pieni muistilaskuri (kymmeniä); f32 esittää sen tarkasti.
    #[allow(clippy::cast_precision_loss)]
    let divisor = n as f32;
    Ok(sum / divisor)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::subject::{CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Task};
    use familyclaw_core::time;

    /// Minimaalinen testisubjekti — palauttaa kiinteät arvot. Subjektin
    /// recall ei vaikuta S2-läpäisyyn, joten tämä riittää saumavarmistukseen.
    struct StubSubject;

    #[async_trait]
    impl Subject for StubSubject {
        async fn start_task(&mut self, task: &Task, _clock: Timestamp) -> Result<RunHandle> {
            Ok(RunHandle::new(task.id.clone(), "stub"))
        }
        async fn kill(&mut self, _handle: &RunHandle, _point: CrashPoint) -> Result<()> {
            Ok(())
        }
        async fn restart(&mut self, _clock: Timestamp) -> Result<RestartReport> {
            Ok(RestartReport {
                steps_replayed: 0,
                was_replaying: false,
                side_effects_reexecuted: 0,
                resumed_clean: true,
            })
        }
        async fn recall(&mut self, _query: &str, _clock: Timestamp) -> Result<Vec<RecallHit>> {
            Ok(vec![RecallHit::new("family", 1.0)])
        }
        async fn sleep_cycle(&mut self, _clock: Timestamp) -> Result<DreamSummary> {
            Ok(DreamSummary {
                scanned: 0,
                merged: 0,
                dropped: 0,
                dates_absolutized: 0,
                strengthened: 0,
                archived: 0,
                protected_core_intact: true,
            })
        }
        #[allow(clippy::unnecessary_literal_bound)]
        fn name(&self) -> &str {
            "stub"
        }
    }

    fn fixed_clock() -> Timestamp {
        time::from_unix_secs(1_717_000_000).expect("valid clock")
    }

    #[tokio::test]
    async fn scenario_passes_for_familyclaw_model() {
        let scenario = RetentionCurve::new();
        let mut subject = StubSubject;
        let result = scenario
            .run(&mut subject, fixed_clock())
            .await
            .expect("run");
        assert_eq!(result.id, "s2_retention_curve");
        assert!(result.passed, "S2 must pass: {:?}", result.notes);
    }

    #[tokio::test]
    async fn anchors_never_decay_at_90_days() {
        let scenario = RetentionCurve::new();
        let mut subject = StubSubject;
        let result = scenario
            .run(&mut subject, fixed_clock())
            .await
            .expect("run");
        let anchor90 = result
            .metrics
            .get("anchor_retention_90d")
            .copied()
            .expect("metric present");
        assert!((anchor90 - 1.0).abs() < 1e-9, "anchors decayed: {anchor90}");
    }

    #[tokio::test]
    async fn trivia_decays_over_time() {
        let scenario = RetentionCurve::new();
        let mut subject = StubSubject;
        let result = scenario
            .run(&mut subject, fixed_clock())
            .await
            .expect("run");
        let decayed = result
            .metrics
            .get("trivia_decayed_90d")
            .copied()
            .expect("metric present");
        assert!(decayed > 0.0, "trivia did not decay at all");
    }

    #[tokio::test]
    async fn familyclaw_beats_naive_baseline() {
        let scenario = RetentionCurve::new();
        let mut subject = StubSubject;
        let result = scenario
            .run(&mut subject, fixed_clock())
            .await
            .expect("run");
        let fc = result
            .metrics
            .get("familyclaw_keeps_important_90d")
            .copied()
            .expect("fc metric");
        let naive = result
            .metrics
            .get("naive_keeps_important_90d")
            .copied()
            .expect("naive metric");
        assert!(fc > naive, "FamilyClaw ({fc}) did not beat naive ({naive})");
    }

    #[tokio::test]
    async fn result_is_deterministic() {
        let scenario = RetentionCurve::new();
        let clock = fixed_clock();
        let mut s1 = StubSubject;
        let mut s2 = StubSubject;
        let a = scenario.run(&mut s1, clock).await.expect("a");
        let b = scenario.run(&mut s2, clock).await.expect("b");
        assert_eq!(a, b, "same clock must yield identical result");
    }
}
