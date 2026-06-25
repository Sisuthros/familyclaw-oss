//! [`Scheduler`]: ajastettujen tehtävien joukko + erääntyneiden lähetys.
//!
//! Ajastin **ei suorita työkaluja itse**. Erääntyneen tehtävän laukaisu
//! reititetään olemassa olevan **idempotentin lähetyksen**
//! ([`familyclaw_actions::ActionRuntime::submit_task_idempotent`]) läpi
//! deterministisellä avaimella ([`crate::decision::firing_key`]). Näin sama
//! looginen laukaisu lähetetään **korkeintaan kerran** myös prosessin
//! kaatumisen yli — koko at-most-once-takuu tulee uudelleenkäytettynä
//! toimintopinosta, ajastin ei keksi sitä uudelleen.
//!
//! Erääntymispäätös tehdään puhtaalla logiikalla ([`crate::decision`]),
//! injektoidulla kellolla — tämä moduuli vain yhdistää sen lähetykseen ja
//! kirjaa `last_fired`-tilan, jotta tehtävä ei laukea uudelleen ennen seuraavaa
//! intervallia.

use std::collections::HashMap;

use familyclaw_actions::{ActionRuntime, Result, SubmitOutcome};
use familyclaw_core::time::Timestamp;

use crate::decision::{decide, due_tasks};
use crate::task::{ScheduledTask, ScheduledTaskId};

/// Yhden tikin lähetysyhteenveto.
///
/// Kertoo kuinka monta tehtävää oli erääntynyt ja lähetettiin tällä tikillä,
/// sekä laukaisukohtaiset lopputulokset (tunniste + idempotenssiavain +
/// lähetyksen tulos). Pelkkä yhteenveto — ei salaisuuksia eikä raakaa payloadia.
#[derive(Debug, Default)]
pub struct DispatchSummary {
    /// Tällä tikillä erääntyneet ja lähetetyt tehtävät: tunniste, avain, tulos.
    pub fired: Vec<(ScheduledTaskId, String, SubmitOutcome)>,
}

impl DispatchSummary {
    /// Tällä tikillä laukaistujen tehtävien lukumäärä.
    #[must_use]
    pub fn fired_count(&self) -> usize {
        self.fired.len()
    }
}

/// Intervalliperustainen ajastin: pitää joukkoa ajastettuja tehtäviä ja niiden
/// viimeisiä laukaisuaikoja, ja lähettää erääntyneet idempotentisti.
///
/// `last_fired`-tila pidetään muistissa; kaatumiskestävyyden idempotenssi tulee
/// **lähetys-outboxista** (deterministinen avain), ei tästä tilasta. Jos
/// `last_fired` nollautuu restartissa, sama intervalli-ikkuna johtaa samaan
/// avaimeen, joten outbox estää kaksoislaukaisun.
#[derive(Debug, Default)]
pub struct Scheduler {
    tasks: Vec<ScheduledTask>,
    last_fired: HashMap<ScheduledTaskId, Timestamp>,
    /// Viimeisin kirjattu ihmisaktiivisuus (perhe-agency: vanhene-jos-ei-
    /// ihmistä, Phase 4). `None` = ihmistä ei ole vielä nähty. Päivitetään
    /// [`Scheduler::note_human_activity`]:lla kun ihminen on aktiivinen.
    last_human_activity: Option<Timestamp>,
}

impl Scheduler {
    /// Luo tyhjän ajastimen.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rekisteröi ajastetun tehtävän.
    ///
    /// Saman tunnisteen uudelleenrekisteröinti **korvaa** aiemman tehtävän
    /// määrittelyn mutta **säilyttää** sen `last_fired`-tilan, jotta intervalli
    /// ei nollaudu vahingossa.
    pub fn register(&mut self, task: ScheduledTask) {
        if let Some(slot) = self.tasks.iter_mut().find(|t| t.id == task.id) {
            *slot = task;
        } else {
            self.tasks.push(task);
        }
    }

    /// Kytkee tehtävän päälle/pois (perhe-agency kill-switch, Phase 4).
    ///
    /// Asettaa [`ScheduledTask::enabled`]-lipun annetulle tehtävälle. `false` =
    /// ajastin ohittaa sen seuraavissa tikeissä; `true` = ottaa taas käyttöön.
    /// Palauttaa `true` jos tehtävä löytyi ja tila asetettiin, `false` jos
    /// tunnistetta ei ole rekisteröity. EI nollaa `last_fired`-tilaa (käyttöön
    /// otto jatkaa normaalia intervallia, ei laukaise heti ellei jo erääntynyt).
    pub fn set_task_enabled(&mut self, id: ScheduledTaskId, enabled: bool) -> bool {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.enabled = enabled;
            true
        } else {
            false
        }
    }

    /// Tehtävän nykyinen enabled-tila (introspektio), tai `None` jos tuntematon.
    #[must_use]
    pub fn task_enabled(&self, id: ScheduledTaskId) -> Option<bool> {
        self.tasks.iter().find(|t| t.id == id).map(|t| t.enabled)
    }

    /// Rekisteröityjen tehtävien tunnisteet (introspektio operaattoripinnalle).
    #[must_use]
    pub fn task_ids(&self) -> Vec<ScheduledTaskId> {
        self.tasks.iter().map(|t| t.id).collect()
    }

    /// Soveltaa persistoidun perhe-agency-configin rekisteröityihin tehtäviin
    /// (Phase 4): configin disabled-listalla olevat otetaan pois käytöstä, loput
    /// jätetään käyttöön. Kutsu **bootissa** rekisteröinnin jälkeen, jotta
    /// operaattorin kill-switch säilyy yli restartin. Tuntemattomat id:t
    /// configissa sivuutetaan vaarattomasti.
    pub fn apply_agency_config(&mut self, config: &crate::persistence::AgencyConfig) {
        for task in &mut self.tasks {
            task.enabled = !config.is_disabled(task.id);
        }
    }

    /// Kirjaa ihmisaktiivisuuden (perhe-agency: vanhene-jos-ei-ihmistä, Phase 4).
    ///
    /// Kutsu kun ihminen on aktiivinen (esim. saapuva ihmisviesti). Päivittää
    /// `last_human_activity`-ajan vain eteenpäin (ei taakse), jotta vanha
    /// aikaleima ei nollaa tuoreempaa. `expire_after_idle`-tehtävät pysyvät
    /// hereillä niin kauan kuin ihminen on ollut aktiivinen tämän ajan sisällä.
    pub fn note_human_activity(&mut self, at: Timestamp) {
        match self.last_human_activity {
            Some(prev) if prev >= at => {}
            _ => self.last_human_activity = Some(at),
        }
    }

    /// Viimeisin kirjattu ihmisaktiivisuus (introspektio), tai `None`.
    #[must_use]
    pub fn last_human_activity(&self) -> Option<Timestamp> {
        self.last_human_activity
    }

    /// Rekisteröityjen tehtävien lukumäärä.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Onko ajastin tyhjä (ei rekisteröityjä tehtäviä).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Palauttaa tehtävän viimeisen laukaisuajan (tai `None`).
    #[must_use]
    pub fn last_fired(&self, id: ScheduledTaskId) -> Option<Timestamp> {
        self.last_fired.get(&id).copied()
    }

    /// Laskee tällä nykyhetkellä erääntyneet tehtävät **suorittamatta** mitään
    /// (puhdas tarkastelu testaukseen ja introspektioon).
    #[must_use]
    pub fn due_now(&self, now: Timestamp) -> Vec<crate::decision::DueDecision> {
        due_tasks(
            &self.tasks,
            |id| self.last_fired(id),
            self.last_human_activity,
            now,
        )
    }

    /// Suorittaa yhden tikin: lähettää kaikki erääntyneet tehtävät
    /// idempotentisti ja kirjaa niiden `last_fired`-ajan.
    ///
    /// Jokainen erääntynyt tehtävä reititetään
    /// [`ActionRuntime::submit_task_idempotent`]:n läpi sen deterministisellä
    /// avaimella ([`crate::decision::firing_key`]) — ajastin ei suorita
    /// työkalua itse. `last_fired` päivitetään vasta onnistuneen lähetyksen
    /// jälkeen, joten ohimenevä virhe ei "kuluta" intervallia: tehtävä yrittää
    /// uudelleen seuraavalla tikillä (ja idempotenssiavain estää
    /// kaksoislaukaisun jos sivuvaikutus oli jo sitoutunut outboxiin).
    ///
    /// # Errors
    /// Palauttaa ensimmäisen lähetysvirheen ([`ActionRuntime::submit_task_idempotent`]).
    /// Sitä ennen onnistuneet laukaisut on jo kirjattu summaariin ja
    /// `last_fired`-tilaan.
    pub async fn tick(
        &mut self,
        runtime: &mut ActionRuntime,
        now: Timestamp,
    ) -> Result<DispatchSummary> {
        let mut summary = DispatchSummary::default();

        // Päätökset lasketaan puhtaalla logiikalla; payload/skill/being haetaan
        // tehtävämäärittelystä lähetystä varten.
        let decisions = self.due_now(now);
        for decision in decisions {
            let Some(key) = decision.key else { continue };
            let Some(task) = self.tasks.iter().find(|t| t.id == decision.task_id) else {
                continue;
            };
            let being = task.being_id.clone();
            let skill_id = task.skill_id;
            let payload = task.payload.clone();
            let task_id = task.id;

            let outcome = runtime
                .submit_task_idempotent(&key, &being, skill_id, payload, now)
                .await?;

            // Kirjaa last_fired vasta onnistuneen lähetyksen jälkeen.
            self.last_fired.insert(task_id, now);
            summary.fired.push((task_id, key, outcome));
        }

        Ok(summary)
    }

    /// Pakottaa yhden tehtävän erääntymispäätöksen tarkasteluun (introspektio).
    #[must_use]
    pub fn decision_for(
        &self,
        id: ScheduledTaskId,
        now: Timestamp,
    ) -> Option<crate::decision::DueDecision> {
        self.tasks
            .iter()
            .find(|t| t.id == id)
            .map(|task| decide(task, self.last_fired(id), self.last_human_activity, now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use familyclaw_actions::skills::FsReadAllowlisted;
    use familyclaw_actions::SkillId;
    use serde_json::json;

    fn at(unix_secs: i64) -> Timestamp {
        familyclaw_core::time::from_unix_secs(unix_secs).expect("valid unix seconds")
    }

    fn runtime_with_fs_read() -> ActionRuntime {
        let mut rt = ActionRuntime::new();
        rt.register_skill(FsReadAllowlisted::new())
            .expect("register fs-read skill");
        rt
    }

    // (3) Idempotentti lähetys: saman tehtävän laukaisu kahdesti samalla
    //     avaimella reitittyy outboxin läpi → sivuvaikutus korkeintaan kerran.
    #[tokio::test]
    async fn second_tick_in_same_window_does_not_refire() {
        let mut rt = runtime_with_fs_read();
        let mut sched = Scheduler::new();

        // fs-read epäonnistuu (tyhjä allowlist) mutta lähetys palauttaa
        // committed-tuloksen outboxiin — riittää todistamaan dedupin.
        let task = ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(11)),
            FsReadAllowlisted::skill_id(),
            json!({"path": "/nonexistent"}),
            Duration::seconds(60),
            "being",
        );
        sched.register(task);

        // Tikki 1: erääntynyt (ei koskaan laukennut) → lähetetään.
        let s1 = sched.tick(&mut rt, at(0)).await.expect("tick 1");
        assert_eq!(s1.fired_count(), 1);

        // Tikki 2 samassa ikkunassa: EI erääntynyt (last_fired = 0, interval 60).
        let s2 = sched.tick(&mut rt, at(30)).await.expect("tick 2");
        assert_eq!(s2.fired_count(), 0);

        // Tikki 3 seuraavassa ikkunassa: erääntynyt taas.
        let s3 = sched.tick(&mut rt, at(60)).await.expect("tick 3");
        assert_eq!(s3.fired_count(), 1);
    }

    #[tokio::test]
    async fn set_task_enabled_toggles_and_reports() {
        let mut sched = Scheduler::new();
        let id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(20));
        let task = ScheduledTask::with_id(
            id,
            FsReadAllowlisted::skill_id(),
            json!({}),
            Duration::seconds(60),
            "being",
        );
        sched.register(task);

        // Oletus enabled.
        assert_eq!(sched.task_enabled(id), Some(true));
        assert_eq!(sched.task_ids(), vec![id]);

        // Kill-switch off.
        assert!(sched.set_task_enabled(id, false), "tunnettu id → true");
        assert_eq!(sched.task_enabled(id), Some(false));

        // Takaisin päälle.
        assert!(sched.set_task_enabled(id, true));
        assert_eq!(sched.task_enabled(id), Some(true));

        // Tuntematon id → false, ei paniikkia.
        let unknown = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(999));
        assert!(!sched.set_task_enabled(unknown, false));
        assert_eq!(sched.task_enabled(unknown), None);
    }

    #[tokio::test]
    async fn idle_task_sleeps_until_human_activity() {
        // Perhe-agency (Phase 4) end-to-end: idle-katollinen tehtävä ei laukea
        // tyhjään huoneeseen, mutta herää kun ihminen on aktiivinen.
        let mut rt = runtime_with_fs_read();
        let mut sched = Scheduler::new();
        let task = ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(40)),
            FsReadAllowlisted::skill_id(),
            json!({"path": "/nonexistent"}),
            Duration::seconds(60),
            "being",
        )
        .with_expire_after_idle(Duration::seconds(100));
        sched.register(task);

        // Ei ihmistä koskaan → idle-vanhentunut → ei laukea vaikka erääntynyt.
        let s1 = sched.tick(&mut rt, at(0)).await.expect("tick idle");
        assert_eq!(s1.fired_count(), 0, "ei laukea tyhjään huoneeseen");

        // Ihminen aktiivinen → herää → laukeaa (sama ikkuna, sama erääntyminen).
        sched.note_human_activity(at(10));
        let s2 = sched.tick(&mut rt, at(10)).await.expect("tick after human");
        assert_eq!(s2.fired_count(), 1, "herää kun ihminen on läsnä");

        // Ihminen poissa kauan (200s idle > 100s katto) → hiljenee taas.
        // (Seuraava ikkuna at(120) jotta erääntyminen olisi muuten ok.)
        let s3 = sched.tick(&mut rt, at(220)).await.expect("tick idle again");
        assert_eq!(s3.fired_count(), 0, "hiljenee taas kun ihminen poissa");
    }

    #[tokio::test]
    async fn apply_agency_config_restores_disabled_state() {
        use crate::persistence::AgencyConfig;
        let mut sched = Scheduler::new();
        let a = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(30));
        let b = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(31));
        for id in [a, b] {
            sched.register(ScheduledTask::with_id(
                id,
                FsReadAllowlisted::skill_id(),
                json!({}),
                Duration::seconds(60),
                "being",
            ));
        }
        // Config disabloi vain a:n (simuloi restartia jossa a oli pysäytetty).
        let mut cfg = AgencyConfig::default();
        cfg.set(a, false);
        sched.apply_agency_config(&cfg);

        assert_eq!(
            sched.task_enabled(a),
            Some(false),
            "a palautui disabloituna"
        );
        assert_eq!(sched.task_enabled(b), Some(true), "b jäi käyttöön");
    }

    #[tokio::test]
    async fn disabled_task_does_not_fire_until_reenabled() {
        // Perhe-agency (Phase 4) end-to-end: disabloitu tehtävä ei lähetä
        // mitään tickissä; käyttöön otto palauttaa laukaisun.
        let mut rt = runtime_with_fs_read();
        let mut sched = Scheduler::new();
        let task = ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(12)),
            FsReadAllowlisted::skill_id(),
            json!({"path": "/nonexistent"}),
            Duration::seconds(60),
            "being",
        )
        .with_enabled(false);
        sched.register(task.clone());

        // Disabloitu → tick ei lähetä, vaikka muuten olisi erääntynyt.
        let s1 = sched.tick(&mut rt, at(0)).await.expect("tick disabled");
        assert_eq!(s1.fired_count(), 0, "disabloitu ei laukea");

        // Ota käyttöön (register korvaa määrittelyn) → laukeaa.
        sched.register(task.with_enabled(true));
        let s2 = sched.tick(&mut rt, at(0)).await.expect("tick enabled");
        assert_eq!(s2.fired_count(), 1, "käyttöön otto palauttaa laukaisun");
    }

    #[tokio::test]
    async fn restart_with_lost_last_fired_dedups_via_outbox_key() {
        // Sama avain samassa ikkunassa: vaikka ajastin "unohtaa" last_fired
        // (uusi Scheduler = restart), idempotenssiavain on sama → outbox
        // palauttaa saman tuloksen ajamatta sivuvaikutusta uudelleen.
        let mut rt = runtime_with_fs_read();
        let task = ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(22)),
            FsReadAllowlisted::skill_id(),
            json!({"path": "/nope"}),
            Duration::seconds(60),
            "being",
        );

        let mut sched_a = Scheduler::new();
        sched_a.register(task.clone());
        let s_a = sched_a.tick(&mut rt, at(70)).await.expect("tick a");
        assert_eq!(s_a.fired_count(), 1);
        let key_a = s_a.fired[0].1.clone();

        // "Restart": uusi ajastin, last_fired kadonnut, sama ikkuna [60,120).
        let mut sched_b = Scheduler::new();
        sched_b.register(task);
        let s_b = sched_b.tick(&mut rt, at(119)).await.expect("tick b");
        assert_eq!(s_b.fired_count(), 1);
        let key_b = s_b.fired[0].1.clone();

        // Sama avain → outbox dedup (sivuvaikutus korkeintaan kerran).
        assert_eq!(key_a, key_b);
    }

    #[tokio::test]
    async fn empty_scheduler_tick_fires_nothing() {
        let mut rt = ActionRuntime::new();
        let mut sched = Scheduler::new();
        let s = sched.tick(&mut rt, at(0)).await.expect("empty tick");
        assert_eq!(s.fired_count(), 0);
    }

    #[tokio::test]
    async fn register_replaces_definition_keeps_last_fired() {
        let mut rt = runtime_with_fs_read();
        let mut sched = Scheduler::new();
        let id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(33));
        sched.register(ScheduledTask::with_id(
            id,
            FsReadAllowlisted::skill_id(),
            json!({"path": "/a"}),
            Duration::seconds(60),
            "b",
        ));
        sched.tick(&mut rt, at(0)).await.expect("tick");
        assert_eq!(sched.last_fired(id), Some(at(0)));

        // Re-register sama id: last_fired säilyy, ei laukea heti uudelleen.
        sched.register(ScheduledTask::with_id(
            id,
            FsReadAllowlisted::skill_id(),
            json!({"path": "/b"}),
            Duration::seconds(60),
            "b",
        ));
        assert_eq!(sched.last_fired(id), Some(at(0)));
        let s = sched.tick(&mut rt, at(30)).await.expect("tick");
        assert_eq!(s.fired_count(), 0);
    }

    #[test]
    fn unknown_skill_decision_still_pure() {
        // decision_for ei lähetä mitään — pelkkä puhdas tarkastelu.
        let mut sched = Scheduler::new();
        let id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(44));
        sched.register(ScheduledTask::with_id(
            id,
            SkillId::new(),
            json!({}),
            Duration::seconds(10),
            "b",
        ));
        let d = sched.decision_for(id, at(0)).expect("decision");
        assert!(d.due);
    }
}
