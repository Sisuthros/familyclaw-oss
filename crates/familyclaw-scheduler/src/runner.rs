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
    /// Ajastin laitetaan `Arc<Mutex<Scheduler>>`:n taakse; tikkisilmukka lukitsee
    /// sen **lyhyesti** per tikki (`tick`-kutsun ajaksi), ja palautettu kahva
    /// sallii esim. gatewayn kytkeä tehtäviä päälle/pois
    /// ([`Scheduler::set_task_enabled`]) saman lukon kautta. Kilpailu ratkeaa
    /// lukolla — ei jaetun tilan kopiointia.
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
                        // Lukko vain tikin ajaksi → operaattorimutaatio mahtuu väliin.
                        let mut sched = loop_handle.lock().await;
                        if let Err(error) = sched.tick(&mut runtime, now).await {
                            tracing::warn!(%error, "ajastimen tikki epäonnistui — jatketaan");
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
}
