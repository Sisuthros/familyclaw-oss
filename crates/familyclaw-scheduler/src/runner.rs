//! [`SchedulerRunner`]: ohut asynkroninen tikkisilmukka — **ainoa** osa joka
//! koskettaa oikeaa aikaa.
//!
//! Runner herää kiinteällä välillä ([`tokio::time::interval`]), kutsuu
//! ajastimen puhdasta erääntymislogiikkaa **oikealla nykyhetkellä** ja lähettää
//! erääntyneet tehtävät idempotentisti ([`Scheduler::tick`]). Päätöslogiikka
//! pysyy puhtaana ja testattavana ilman oikeaa aikaa — runner vain syöttää sille
//! kellon.
//!
//! ## Peruutettavuus (kill switch)
//! Silmukka pysähtyy siististi kun [`CancellationSignal`] laukaistaan
//! (`cancel()`-kutsu **tai** sen pudottaminen). Toteutus käyttää
//! [`tokio::sync::watch`]-kanavaa: lähettäjän pudottaminen sulkee kanavan ja
//! silmukka näkee sen → pysähtyy. Näin sekä eksplisiittinen sammutussignaali
//! että kahvan pudottaminen lopettavat ajastimen.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use familyclaw_actions::ActionRuntime;
use familyclaw_core::time::Timestamp;
use tokio::sync::{watch, Mutex};

use crate::dispatch::Scheduler;

/// Jaettu kahva ajastimeen ajon aikana (perhe-agency operaattoripinnalle).
///
/// [`SchedulerRunner::run_shared`] palauttaa tämän, jotta tikkisilmukan rinnalla
/// esim. gateway voi kytkeä tehtäviä päälle/pois
/// ([`Scheduler::set_task_enabled`]). Lukko otetaan **lyhyesti** sekä tikissä
/// (per erääntymisarvio) että operaattorimutaatiossa — ei pitkiä pitoja.
pub type SchedulerHandle = Arc<Mutex<Scheduler>>;

/// Peruutussignaali ajastinsilmukalle (kill switch).
///
/// Säilytä tämä kahva ajastimen ulkopuolella. [`CancellationSignal::cancel`]
/// (tai kahvan pudottaminen) pysäyttää silmukan siististi seuraavalla tikillä
/// tai välittömästi jos se odottaa.
#[derive(Debug)]
pub struct CancellationSignal {
    tx: watch::Sender<bool>,
}

impl CancellationSignal {
    /// Pyytää silmukkaa pysähtymään.
    ///
    /// Idempotentti: useampi kutsu on turvallinen. Vaikutus on sama kuin
    /// kahvan pudottaminen.
    pub fn cancel(&self) {
        // Lähetysvirhe tarkoittaa että vastaanottaja on jo pudonnut (silmukka
        // lopetti) — silloin ei ole mitään pysäytettävää.
        let _ = self.tx.send(true);
    }
}

/// Sisäinen peruutuksen vastaanottopää, jonka silmukka pollaa.
#[derive(Debug, Clone)]
struct CancellationToken {
    rx: watch::Receiver<bool>,
}

impl CancellationToken {
    /// Onko peruutusta pyydetty (joko `cancel()` tai lähettäjä pudonnut).
    fn is_cancelled(&self) -> bool {
        // Suljettu kanava (lähettäjä pudonnut) ⇒ peruutettu. Muuten lue lippu.
        if self.rx.has_changed().is_err() {
            return true;
        }
        *self.rx.borrow()
    }

    /// Odottaa kunnes peruutus laukeaa (lippu tai kanavan sulkeutuminen).
    async fn cancelled(&mut self) {
        loop {
            if *self.rx.borrow() {
                return;
            }
            // `changed()` palauttaa Err kun lähettäjä on pudonnut → peruutettu.
            if self.rx.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Luo peruutussignaali–token-parin.
#[must_use]
fn cancellation_pair() -> (CancellationSignal, CancellationToken) {
    let (tx, rx) = watch::channel(false);
    (CancellationSignal { tx }, CancellationToken { rx })
}

/// Asynkroninen ajastinsilmukka jonka voi peruuttaa.
///
/// Runner omistaa [`Scheduler`]:n ja [`ActionRuntime`]:n ajon ajan ja tikittää
/// niitä kiinteällä välillä. Aloita ajo [`SchedulerRunner::run`]:lla; se palaa
/// vasta kun silmukka peruutetaan.
#[derive(Debug)]
pub struct SchedulerRunner {
    scheduler: Scheduler,
    runtime: ActionRuntime,
    period: StdDuration,
}

impl SchedulerRunner {
    /// Luo runnerin annetulla ajastimella, toimintoajoympäristöllä ja
    /// tikkivälillä.
    ///
    /// `period` on **runnerin** herätysväli (kuinka usein erääntymistä
    /// arvioidaan) — eri asia kuin yksittäisen tehtävän intervalli. Pidä se
    /// pienempänä tai yhtä suurena kuin lyhin tehtäväintervalli, jotta
    /// erääntymiset huomataan ajoissa.
    #[must_use]
    pub fn new(scheduler: Scheduler, runtime: ActionRuntime, period: StdDuration) -> Self {
        Self {
            scheduler,
            runtime,
            period,
        }
    }

    /// Ajaa tikkisilmukkaa kunnes `cancel` laukeaa.
    ///
    /// Palauttaa peruutussignaalin (kill switch) jonka kautta silmukka
    /// pysäytetään. `now_fn` injektoi nykyhetken **tikin sisällä** — tuotannossa
    /// [`familyclaw_core::time::now`], testissä ohjattava kello. Silmukka itse
    /// käyttää oikeaa aikaa vain [`tokio::time::interval`]:n kautta; mitä
    /// *kelloa* tehtäthe operator annetaan, tulee `now_fn`:stä, joten erääntymislogiikka
    /// pysyy testattavana.
    ///
    /// Lähetysvirheet ([`Scheduler::tick`]) lokitetaan ja silmukka **jatkaa** —
    /// yhden tehtävän ohimenevä virhe ei kaada koko ajastinta.
    ///
    /// Vaatii että kutsutaan Tokio-ajoympäristön sisältä
    /// ([`tokio::spawn`]:ia varten).
    pub fn run<F>(self, now_fn: F) -> CancellationSignal
    where
        F: Fn() -> Timestamp + Send + 'static,
    {
        let (signal, token) = cancellation_pair();
        let mut scheduler = self.scheduler;
        let mut runtime = self.runtime;
        let period = self.period;
        let mut token_loop = token;

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(period);
            loop {
                tokio::select! {
                    biased;
                    () = token_loop.cancelled() => {
                        break;
                    }
                    _ = ticker.tick() => {
                        if token_loop.is_cancelled() {
                            break;
                        }
                        let now = now_fn();
                        if let Err(error) = scheduler.tick(&mut runtime, now).await {
                            tracing::warn!(%error, "ajastimen tikki epäonnistui — jatketaan");
                        }
                    }
                }
            }
        });

        signal
    }

    /// Kuten [`run`](Self::run), mutta palauttaa myös **jaetun kahvan** ajastimeen
    /// ([`SchedulerHandle`]) operaattoripinnalle (perhe-agency, Phase 4).
    ///
    /// Ajastin laitetaan `Arc<Mutex<Scheduler>>`:n taakse, ja palautettu kahva
    /// sallii esim. gatewayn kytkeä tehtäviä päälle/pois
    /// ([`Scheduler::set_task_enabled`]) saman lukon kautta.
    ///
    /// ## Lukkoa EI pidetä lähetyksen (`await`) yli
    /// Jokainen tikki tekee kolme vaihetta: **(1)** ottaa lukon **vain hetkeksi**
    /// ja kerää erääntyneet lähetysohjeet ([`Scheduler::collect_due`], puhdas, ei
    /// `await`), **(2)** vapauttaa lukon ja ajaa idempotentit lähetykset
    /// ([`ActionRuntime::submit_task_idempotent`]) **ilman lukkoa**, **(3)** ottaa
    /// lukon taas lyhyesti kirjatakseen `last_fired`:n onnistuneille
    /// ([`Scheduler::record_fired`]). Näin pitkä lähetys-I/O **ei** estä
    /// operaattoripinnan mutaatioita (pause/resume/kill-switch) — ne mahtuvat
    /// väliin vaiheiden 2 aikana, kun lukko on vapaana. Aiemmin lukko pidettiin
    /// koko `tick().await`:n yli, jolloin gateway-komennot jonottivat hitaan
    /// tikin taakse.
    ///
    /// Erääntymispäätös pysyy oikeana: avain ([`crate::decision::firing_key`]) on
    /// vakaa intervalli-ikkunan sisällä ja lähetys on idempotentti, joten vaikka
    /// tehtävä kytkettäisiin pois lähetyksen aikana, jo aloitettu laukaisu menee
    /// loppuun korkeintaan kerran eikä `last_fired`-kirjaus riko seuraavaa
    /// ikkunaa.
    ///
    /// Vaatii Tokio-ajoympäristön ([`tokio::spawn`]).
    pub fn run_shared<F>(self, now_fn: F) -> (CancellationSignal, SchedulerHandle)
    where
        F: Fn() -> Timestamp + Send + 'static,
    {
        let (signal, token) = cancellation_pair();
        let handle: SchedulerHandle = Arc::new(Mutex::new(self.scheduler));
        let loop_handle = Arc::clone(&handle);
        let mut runtime = self.runtime;
        let period = self.period;
        let mut token_loop = token;

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(period);
            loop {
                tokio::select! {
                    biased;
                    () = token_loop.cancelled() => {
                        break;
                    }
                    _ = ticker.tick() => {
                        if token_loop.is_cancelled() {
                            break;
                        }
                        let now = now_fn();

                        // (1) Lukko vain päätöksen ajaksi: kerää erääntyneet
                        //     lähetysohjeet (puhdas, ei await) ja vapauta lukko.
                        let due = {
                            let sched = loop_handle.lock().await;
                            sched.collect_due(now)
                        };

                        // (2) Lähetä ILMAN lukkoa → operaattorimutaatiot mahtuvat
                        //     väliin pitkänkin lähetyksen aikana.
                        for dispatch in due {
                            let result = runtime
                                .submit_task_idempotent(
                                    &dispatch.key,
                                    &dispatch.being_id,
                                    dispatch.skill_id,
                                    dispatch.payload,
                                    now,
                                )
                                .await;
                            match result {
                                Ok(_) => {
                                    // (3) Lukko taas lyhyesti vain kirjausta varten.
                                    loop_handle.lock().await.record_fired(dispatch.task_id, now);
                                }
                                Err(error) => {
                                    tracing::warn!(%error, "ajastimen lähetys epäonnistui — jatketaan");
                                }
                            }
                        }
                    }
                }
            }
        });

        (signal, handle)
    }
}

/// Mukavuusfunktio: ajaa runnerin ja palauttaa peruutussignaalin.
///
/// Vastaa [`SchedulerRunner::run`]:ia oletuskellolla
/// ([`familyclaw_core::time::now`]). Käytä [`SchedulerRunner::run`]:ia suoraan
/// jos haluat injektoida kellon testissä. Vaatii Tokio-ajoympäristön.
#[must_use]
pub fn run_until_cancelled(runner: SchedulerRunner) -> CancellationSignal {
    runner.run(familyclaw_core::time::now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn now_at_secs(secs: i64) -> Timestamp {
        familyclaw_core::time::from_unix_secs(secs).expect("valid unix seconds")
    }

    // (4) Runner on peruutettavissa: käynnistä, peruuta, varmista pysähtyminen.
    #[tokio::test(start_paused = true)]
    async fn runner_stops_after_explicit_cancel() {
        // Laske kuinka monta kertaa now_fn kutsutaan (= tikkien määrä).
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = Arc::clone(&calls);

        let runner = SchedulerRunner::new(
            Scheduler::new(),
            ActionRuntime::new(),
            StdDuration::from_millis(10),
        );
        let signal = runner.run(move || {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            now_at_secs(0)
        });

        // Anna muutaman tikin tapahtua paused-ajassa.
        tokio::time::advance(StdDuration::from_millis(35)).await;
        tokio::task::yield_now().await;
        let before = calls.load(Ordering::SeqCst);
        assert!(before >= 1, "silmukan piti tikittää ainakin kerran");

        // Peruuta ja varmista että silmukka pysähtyy (ei lisää tikkejä).
        signal.cancel();
        tokio::task::yield_now().await;
        tokio::time::advance(StdDuration::from_millis(100)).await;
        tokio::task::yield_now().await;
        let after = calls.load(Ordering::SeqCst);

        tokio::time::advance(StdDuration::from_millis(100)).await;
        tokio::task::yield_now().await;
        let final_count = calls.load(Ordering::SeqCst);
        assert_eq!(
            after, final_count,
            "peruutuksen jälkeen ei saa tikittää lisää"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn runner_stops_when_signal_dropped() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = Arc::clone(&calls);

        let runner = SchedulerRunner::new(
            Scheduler::new(),
            ActionRuntime::new(),
            StdDuration::from_millis(10),
        );
        let signal = runner.run(move || {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            now_at_secs(0)
        });

        tokio::time::advance(StdDuration::from_millis(25)).await;
        tokio::task::yield_now().await;

        // Pudota kahva → kanava sulkeutuu → silmukka pysähtyy.
        drop(signal);
        tokio::task::yield_now().await;
        tokio::time::advance(StdDuration::from_millis(50)).await;
        tokio::task::yield_now().await;
        let after = calls.load(Ordering::SeqCst);

        tokio::time::advance(StdDuration::from_millis(100)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            after,
            calls.load(Ordering::SeqCst),
            "pudotuksen jälkeen ei lisää tikkejä"
        );
    }

    #[test]
    fn cancel_is_idempotent() {
        let (signal, token) = cancellation_pair();
        signal.cancel();
        signal.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn dropped_signal_marks_token_cancelled() {
        let (signal, token) = cancellation_pair();
        assert!(!token.is_cancelled());
        drop(signal);
        assert!(token.is_cancelled());
    }

    // ── Lukko-ei-pidossa-await-yli -testit (PR: control-plane ei jonota tikin
    //    taakse) ────────────────────────────────────────────────────────────
    //
    // Nämä testit käyttävät OIKEAA aikaa (ei start_paused) ja multi-thread-
    // runtimea, koska ne mittaavat aitoa rinnakkaisuutta runnerin tikkisilmukan
    // ja ulkoisen operaattorimutaation välillä jaetun lukon kautta.

    use std::time::Duration as RealDuration;

    use async_trait::async_trait;
    use chrono::Duration as ChronoDuration;
    use familyclaw_actions::executor::{ActionExecutor, ActionRequest, ActionResult};
    use familyclaw_actions::manifest::SkillManifest;
    use familyclaw_actions::policy::{ActionRisk, ApprovalPolicy, SkillPermission};
    use familyclaw_actions::skills::Skill;
    use familyclaw_actions::SkillId;
    use tokio::sync::Notify;

    use crate::task::{ScheduledTask, ScheduledTaskId};

    /// Testitaito jonka suoritus **jää odottamaan** hallittua vapautusbarriääriä.
    ///
    /// `execute` ilmoittaa ensin että suoritus on alkanut (`started`), laskee
    /// suorituskerrat (`run_count`), ja jää sitten odottamaan `release`-
    /// barriäriä ennen palaamista. Näin testi voi pitää yhden tikin lähetyksen
    /// "käynnissä" ja todistaa että operaattorimutaatio mahtuu väliin lukon
    /// ollessa vapaana.
    #[derive(Debug)]
    struct BarrierSkill {
        id: SkillId,
        started: Arc<Notify>,
        release: Arc<Notify>,
        run_count: Arc<AtomicUsize>,
    }

    impl BarrierSkill {
        fn new(id: SkillId) -> (Self, Arc<Notify>, Arc<Notify>, Arc<AtomicUsize>) {
            let started = Arc::new(Notify::new());
            let release = Arc::new(Notify::new());
            let run_count = Arc::new(AtomicUsize::new(0));
            let skill = Self {
                id,
                started: Arc::clone(&started),
                release: Arc::clone(&release),
                run_count: Arc::clone(&run_count),
            };
            (skill, started, release, run_count)
        }
    }

    #[async_trait]
    impl ActionExecutor for BarrierSkill {
        async fn execute(
            &self,
            request: ActionRequest,
        ) -> familyclaw_actions::Result<ActionResult> {
            self.run_count.fetch_add(1, Ordering::SeqCst);
            // Ilmoita että suoritus on alkanut, jää sitten odottamaan vapautusta.
            self.started.notify_one();
            self.release.notified().await;
            Ok(ActionResult::success(
                "barrier skill released",
                serde_json::Value::Null,
                request.now,
            ))
        }
    }

    impl Skill for BarrierSkill {
        fn manifest(&self) -> SkillManifest {
            SkillManifest {
                id: self.id,
                name: "barrier_test_skill".to_string(),
                version: "1.0.0".to_string(),
                description: "Test skill that blocks on a controllable barrier.".to_string(),
                permissions: vec![SkillPermission::ReadFiles],
                risk: ActionRisk::ReadOnly,
                approval_policy: ApprovalPolicy::AutoIfReadOnly,
                input_hint: None,
                output_hint: None,
                input_schema: serde_json::json!({ "type": "object" }),
                publisher: None,
                signature: None,
            }
        }
    }

    fn barrier_task(id: ScheduledTaskId, skill_id: SkillId) -> ScheduledTask {
        ScheduledTask::with_id(
            id,
            skill_id,
            serde_json::json!({}),
            ChronoDuration::seconds(60),
            "being",
        )
    }

    // (core) Operaattorimutaatio (set_task_enabled) valmistuu SAMALLA kun pitkä
    // tikin lähetys on käynnissä — todistaa ettei lukkoa pidetä await-yli.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn control_mutation_completes_while_long_action_in_progress() {
        let skill_id = SkillId::new();
        let (skill, started, release, _run_count) = BarrierSkill::new(skill_id);

        let mut runtime = ActionRuntime::new();
        runtime
            .register_skill(skill)
            .expect("register barrier skill");

        let mut sched = Scheduler::new();
        let task_id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(101));
        let other_id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(102));
        sched.register(barrier_task(task_id, skill_id));
        sched.register(barrier_task(other_id, skill_id));

        let runner = SchedulerRunner::new(sched, runtime, RealDuration::from_millis(5));
        let (signal, handle) = runner.run_shared(|| now_at_secs(0));

        // Odota että ensimmäisen erääntyneen tehtävän lähetys on KÄYNNISSÄ
        // (skill jäi barriäriin). Tässä pisteessä lähetyssilmukka on await:ssa
        // EIKÄ pidä ajastimen lukkoa (se kerättiin ja vapautettiin ennen await:ia).
        tokio::time::timeout(RealDuration::from_secs(5), started.notified())
            .await
            .expect("barrier skill should have started");

        // Operaattorimutaatio: pitää valmistua VÄLITTÖMÄSTI vaikka lähetys on
        // käynnissä — lukko on vapaana await:n aikana.
        let mutation = tokio::time::timeout(RealDuration::from_secs(2), async {
            let mut s = handle.lock().await;
            s.set_task_enabled(other_id, false)
        })
        .await;
        assert!(
            mutation.is_ok(),
            "set_task_enabled jumiutui lähetyksen taakse — lukko pidettiin await-yli"
        );
        assert!(
            mutation.unwrap(),
            "tunnettu id → set_task_enabled palauttaa true"
        );

        // Vapauta barriäri ettei runtime jää roikkumaan, ja pysäytä silmukka.
        release.notify_waiters();
        signal.cancel();
    }

    // (dispatch-once) Erääntynyt tehtävä lähetetään täsmälleen kerran per ikkuna.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_runner_dispatches_due_task_exactly_once() {
        let skill_id = SkillId::new();
        let (skill, started, release, run_count) = BarrierSkill::new(skill_id);
        // Vapauta barriäri heti jokaiselle odottajalle, jotta lähetys palaa
        // välittömästi (ei jää roikkumaan) — tämä testi mittaa laukaisukertoja.
        let release_for_task = Arc::clone(&release);
        tokio::spawn(async move {
            loop {
                release_for_task.notify_waiters();
                tokio::time::sleep(RealDuration::from_millis(1)).await;
            }
        });

        let mut runtime = ActionRuntime::new();
        runtime.register_skill(skill).expect("register");

        let mut sched = Scheduler::new();
        let task_id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(111));
        sched.register(barrier_task(task_id, skill_id));

        // Kiinteä now → sama intervalli-ikkuna joka tikillä; idempotenssiavain on
        // sama, joten useampi tikki samassa ikkunassa EI saa laukaista uudelleen.
        let runner = SchedulerRunner::new(sched, runtime, RealDuration::from_millis(3));
        let (signal, _handle) = runner.run_shared(|| now_at_secs(1000));

        // Odota ensimmäinen laukaisu.
        tokio::time::timeout(RealDuration::from_secs(5), started.notified())
            .await
            .expect("task should fire once");
        // Anna monta tikkiä kulua samassa ikkunassa.
        tokio::time::sleep(RealDuration::from_millis(60)).await;
        signal.cancel();
        tokio::time::sleep(RealDuration::from_millis(20)).await;

        assert_eq!(
            run_count.load(Ordering::SeqCst),
            1,
            "sama ikkuna → outbox dedup → täsmälleen yksi laukaisu"
        );
    }

    // (disabled stays quiet) Pois käytöstä otettu tehtävä ei lähetä mitään.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_runner_disabled_task_stays_quiet() {
        let skill_id = SkillId::new();
        let (skill, _started, release, run_count) = BarrierSkill::new(skill_id);
        release.notify_waiters(); // ei odottajia vielä; varmuuden vuoksi

        let mut runtime = ActionRuntime::new();
        runtime.register_skill(skill).expect("register");

        let mut sched = Scheduler::new();
        let task_id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(121));
        sched.register(barrier_task(task_id, skill_id).with_enabled(false));

        let runner = SchedulerRunner::new(sched, runtime, RealDuration::from_millis(3));
        let (signal, _handle) = runner.run_shared(|| now_at_secs(0));

        // Anna useita tikkejä kulua — disabloitu tehtävä ei saa laukaista.
        tokio::time::sleep(RealDuration::from_millis(60)).await;
        signal.cancel();
        tokio::time::sleep(RealDuration::from_millis(10)).await;

        assert_eq!(
            run_count.load(Ordering::SeqCst),
            0,
            "disabloitu tehtävä ei laukea jaetussa runnerissa"
        );
    }

    // (cancellation) Jaettu runner pysähtyy cancel-signaalilla myös kun lähetys
    // ei pidä lukkoa.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_runner_stops_on_cancel() {
        let skill_id = SkillId::new();
        let (skill, _started, release, run_count) = BarrierSkill::new(skill_id);
        let release_for_task = Arc::clone(&release);
        tokio::spawn(async move {
            loop {
                release_for_task.notify_waiters();
                tokio::time::sleep(RealDuration::from_millis(1)).await;
            }
        });

        let mut runtime = ActionRuntime::new();
        runtime.register_skill(skill).expect("register");

        let mut sched = Scheduler::new();
        sched.register(barrier_task(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(131)),
            skill_id,
        ));

        let runner = SchedulerRunner::new(sched, runtime, RealDuration::from_millis(3));
        let (signal, _handle) = runner.run_shared(|| now_at_secs(0));

        tokio::time::sleep(RealDuration::from_millis(30)).await;
        signal.cancel();
        // Anna silmukan nähdä peruutus ja pysähtyä.
        tokio::time::sleep(RealDuration::from_millis(20)).await;
        let after_cancel = run_count.load(Ordering::SeqCst);
        tokio::time::sleep(RealDuration::from_millis(40)).await;
        assert_eq!(
            run_count.load(Ordering::SeqCst),
            after_cancel,
            "peruutuksen jälkeen ei uusia laukaisuja"
        );
    }

    // (no deadlock) Rinnakkaiset set_task_enabled-kutsut tikin kanssa eivät
    // lukkiudu: silmukka jatkaa lähetystä lukon ollessa pääosin vapaa.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shared_runner_no_deadlock_under_concurrent_mutations() {
        let skill_id = SkillId::new();
        let (skill, _started, release, _run_count) = BarrierSkill::new(skill_id);
        let release_for_task = Arc::clone(&release);
        tokio::spawn(async move {
            loop {
                release_for_task.notify_waiters();
                tokio::time::sleep(RealDuration::from_millis(1)).await;
            }
        });

        let mut runtime = ActionRuntime::new();
        runtime.register_skill(skill).expect("register");

        let mut sched = Scheduler::new();
        let ids: Vec<ScheduledTaskId> = (0..5)
            .map(|n| ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(200 + n)))
            .collect();
        for id in &ids {
            sched.register(barrier_task(*id, skill_id));
        }

        let runner = SchedulerRunner::new(sched, runtime, RealDuration::from_millis(2));
        let (signal, handle) = runner.run_shared(|| now_at_secs(0));

        // Hakkaa operaattorimutaatioita rinnakkain tikin kanssa.
        let hammer = {
            let handle = Arc::clone(&handle);
            let ids = ids.clone();
            tokio::spawn(async move {
                for round in 0..200u32 {
                    let mut s = handle.lock().await;
                    for id in &ids {
                        s.set_task_enabled(*id, round % 2 == 0);
                    }
                    drop(s);
                    tokio::task::yield_now().await;
                }
            })
        };

        // Koko homma pitää valmistua reilusti aikarajan sisällä (ei deadlockia).
        let done = tokio::time::timeout(RealDuration::from_secs(10), hammer).await;
        assert!(
            done.is_ok(),
            "rinnakkaiset mutaatiot lukkiutuivat (deadlock)"
        );
        done.unwrap().expect("hammer task panicked");

        signal.cancel();
    }
}
