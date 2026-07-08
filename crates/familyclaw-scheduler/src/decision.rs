//! **Puhdas** erääntymis- ja avainlogiikka (injektoitu kello).
//!
//! Tässä moduulissa ei lueta oikeaa kelloa: nykyhetki annetaan aina
//! injektoituna ([`Timestamp`]). Siksi koko logiikka — *mitkä* tehtävät
//! erääntyvät ja *millä* idempotenssiavaimella ne laukeavat — on
//! yksikkötestattavissa ilman oikeaa aikaa. Vain [`crate::runner`] koskettaa
//! [`tokio::time`]:a.
//!
//! ## Erääntymissääntö
//! Tehtävä on **erääntynyt** kun
//! - **intervalli:** `now >= last_fired + interval` (oletus), tai
//! - **cron:** nykyhetkeen `<= now` osuva cron-esiintymä on uudempi kuin
//!   `last_fired` (ks. [`is_due_cron`]).
//!
//! Jos tehtävä ei ole koskaan laukennut (`last_fired = None`), se on erääntynyt
//! heti ensimmäisellä arvioinnilla (intervalli) tai kun cron-esiintymä on
//! saavutettu (cron). Ei-positiivinen intervalli kohdellaan "aina erääntyneenä"
//! (turvallinen degeneraatio; tuotannossa intervallin oletetaan olevan
//! positiivinen).
//!
//! ## Idempotenssiavaimen vakaus
//! Laukaisun avain on `schedule-{task_id}-{epoch_bucket}` (intervalli) tai
//! `schedule-{task_id}-{occurrence_unix}` (cron), jossa `occurrence_unix` on
//! cron-esiintymän Unix-aika. Saman intervalli-ikkunan tai cron-esiintymän
//! sisällä **mikä tahansa** `now` tuottaa saman avaimen.

use chrono::Duration;
use croner::Cron;
use familyclaw_core::time::Timestamp;
use std::str::FromStr;

use crate::task::{ScheduledTask, ScheduledTaskId};

/// Yhden tehtävän erääntymispäätös tietyllä nykyhetkellä.
///
/// Sisältää tehtävän tunnisteen, erääntymistilan ja — jos erääntynyt — sille
/// johdetun deterministisen idempotenssiavaimen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueDecision {
    /// Tehtävän tunniste johon päätös viittaa.
    pub task_id: ScheduledTaskId,
    /// Onko tehtävä erääntynyt (laukeaako tällä `now`-arvolla).
    pub due: bool,
    /// Deterministinen idempotenssiavain tälle laukaisulle, jos `due == true`.
    ///
    /// Avain on vakaa yli restartin samalle loogiselle laukaisuikkunalle (ks.
    /// [`firing_key`]); ei-erääntyneellä tehtävällä tämä on `None`.
    pub key: Option<String>,
}

/// Johtaa **deterministisen** idempotenssiavaimen yhdelle laukaisulle.
///
/// Avain on `schedule-{task_id}-{epoch_bucket}`, jossa
/// `epoch_bucket = floor(now_unix / interval_secs)`. Ominaisuudet:
///
/// - **Vakaa ikkunan sisällä:** kaikki `now`-arvot samassa
///   `[bucket*interval, (bucket+1)*interval)`-ikkunassa tuottavat saman
///   avaimen → kaatumis-/restart-uudelleenarviointi osuu lähetys-outboxissa jo
///   sitoutuneeseen avaimeen eikä laukaise sivuvaikutusta toiseen kertaan.
/// - **Riippumaton prosessin muistista:** johdetaan pelkästään `task_id`:stä,
///   intervallista ja nykyhetkestä — ei tikki-laskureista joita restart
///   nollaisi.
/// - **Vaihtuu ikkunan yli:** seuraava intervalli-ikkuna saa uuden bucketin →
///   seuraava laukaisu on eri avain, joten se ei deduploidu väärin edelliseen.
///
/// Ei-positiivinen intervalli kohdellaan yhden sekunnin ikkunana avaimen
/// johtamisessa, jotta avain pysyy hyvin määriteltynä degeneroituneessakin
/// tapauksessa (erääntyminen itse hoidetaan erikseen [`is_due`]:ssä).
#[must_use]
pub fn firing_key(task_id: ScheduledTaskId, interval: Duration, now: Timestamp) -> String {
    let interval_secs = interval.num_seconds().max(1);
    let now_secs = now.timestamp();
    let bucket = now_secs.div_euclid(interval_secs);
    format!("schedule-{task_id}-{bucket}")
}

/// Jäsentää cron-lausekkeen. Palauttaa `None` virheelliselle lausekkeelle.
#[must_use]
pub fn parse_cron(expression: &str) -> Option<Cron> {
    Cron::from_str(expression).ok()
}

/// Palauttaa nykyhetkeen `<= now` osuvan viimeisimmän cron-esiintymän.
#[must_use]
pub fn cron_occurrence_at(expression: &str, now: Timestamp) -> Option<Timestamp> {
    let cron = parse_cron(expression)?;
    cron.find_previous_occurrence(&now, true).ok()
}

/// Onko cron-tehtävä erääntynyt annetulla nykyhetkellä.
///
/// `last_fired = None` → erääntynyt kun cron-esiintymä on löydettävissä.
/// Muuten erääntynyt kun viimeisin cron-esiintymä `<= now` on uudempi kuin
/// `last_fired`. Virheellinen lauseke → ei koskaan erääntynyt (fail-closed).
#[must_use]
pub fn is_due_cron(expression: &str, last_fired: Option<Timestamp>, now: Timestamp) -> bool {
    let Some(occurrence) = cron_occurrence_at(expression, now) else {
        return false;
    };
    match last_fired {
        None => true,
        Some(last) => last < occurrence,
    }
}

/// Johtaa deterministisen idempotenssiavaimen yhdelle cron-laukaisulle.
///
/// Avain on `schedule-{task_id}-{occurrence_unix}`, jossa `occurrence_unix` on
/// [`cron_occurrence_at`]:n palauttama esiintymä. Virheellinen lauseke palauttaa
/// avaimen suffiksilla `invalid` (ei-erääntynyt tehtävä ei käytä sitä).
#[must_use]
pub fn cron_firing_key(task_id: ScheduledTaskId, expression: &str, now: Timestamp) -> String {
    match cron_occurrence_at(expression, now) {
        Some(occurrence) => format!("schedule-{task_id}-{}", occurrence.timestamp()),
        None => format!("schedule-{task_id}-invalid"),
    }
}

/// Onko tehtävä erääntynyt annetulla nykyhetkellä.
///
/// `last_fired = None` tarkoittaa "ei koskaan laukennut" → erääntynyt heti.
/// Muuten erääntynyt kun `now >= last_fired + interval`. Ei-positiivinen
/// intervalli → aina erääntynyt.
#[must_use]
pub fn is_due(interval: Duration, last_fired: Option<Timestamp>, now: Timestamp) -> bool {
    if interval <= Duration::zero() {
        return true;
    }
    match last_fired {
        None => true,
        Some(last) => now >= last + interval,
    }
}

/// Arvioi yhden tehtävän erääntymispäätöksen ([`DueDecision`]).
///
/// Puhdas funktio: ei sivuvaikutuksia, kello injektoituna. Jos tehtävä on
/// erääntynyt, palautettu päätös sisältää sille johdetun deterministisen
/// avaimen ([`firing_key`]).
#[must_use]
pub fn decide(
    task: &ScheduledTask,
    last_fired: Option<Timestamp>,
    last_human_activity: Option<Timestamp>,
    now: Timestamp,
) -> DueDecision {
    // Perhe-agency (Phase 4): pois käytöstä otettu tehtävä EI laukea koskaan —
    // ihmisen kill-switch ohittaa erääntymisen kokonaan. JA: vanhene-jos-ei-
    // ihmistä — jos idle-katto on asetettu ja ihmisaktiivisuudesta on kulunut
    // liikaa, tehtävä hiljenee (ei laukea), kunnes ihminen palaa.
    let schedule_due = if let Some(ref cron) = task.cron_expression {
        is_due_cron(cron, last_fired, now)
    } else {
        is_due(task.interval, last_fired, now)
    };
    let due = task.enabled
        && !idle_expired(task.expire_after_idle, last_human_activity, now)
        && schedule_due;
    let key = if due {
        if let Some(ref cron) = task.cron_expression {
            Some(cron_firing_key(task.id, cron, now))
        } else {
            Some(firing_key(task.id, task.interval, now))
        }
    } else {
        None
    };
    DueDecision {
        task_id: task.id,
        due,
        key,
    }
}

/// Onko tehtävä **vanhentunut idleen** (perhe-agency: vanhene-jos-ei-ihmistä).
///
/// `expire_after_idle = None` → ei koskaan vanhene (palauttaa `false`). Muuten
/// vanhentunut kun `now - last_human_activity > expire_after_idle`. Jos
/// ihmisaktiivisuutta ei ole koskaan kirjattu (`None`), tehtävä on vanhentunut
/// heti idle-katon ollessa asetettu — proaktiivinen tehtävä ei käynnisty tyhjään
/// huoneeseen ennen kuin ihminen on edes kerran ollut läsnä.
#[must_use]
pub fn idle_expired(
    expire_after_idle: Option<Duration>,
    last_human_activity: Option<Timestamp>,
    now: Timestamp,
) -> bool {
    let Some(idle) = expire_after_idle else {
        return false; // ei idle-kattoa → ei koskaan vanhene
    };
    if idle <= Duration::zero() {
        return false; // ei-positiivinen katto → ei vanhene (turvallinen degeneraatio)
    }
    match last_human_activity {
        None => true, // ei koskaan ihmistä + idle-katto asetettu → vanhentunut
        Some(last) => now > last + idle,
    }
}

/// Laskee erääntyneet tehtävät listalle kerralla.
///
/// `last_fired` on hakulausekefunktio joka palauttaa tehtävän viimeisen
/// laukaisuajan (tai `None` jos ei koskaan laukennut). `last_human_activity` on
/// viimeisin kirjattu ihmisaktiivisuus (idle-vanhenemista varten; `None` =
/// ihmistä ei ole vielä nähty). Palauttaa vain erääntyneiden tehtävien
/// päätökset, syötteen järjestyksessä (deterministinen).
#[must_use]
pub fn due_tasks<F>(
    tasks: &[ScheduledTask],
    mut last_fired: F,
    last_human_activity: Option<Timestamp>,
    now: Timestamp,
) -> Vec<DueDecision>
where
    F: FnMut(ScheduledTaskId) -> Option<Timestamp>,
{
    tasks
        .iter()
        .map(|task| decide(task, last_fired(task.id), last_human_activity, now))
        .filter(|decision| decision.due)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_actions::SkillId;
    use serde_json::json;

    fn at(unix_secs: i64) -> Timestamp {
        familyclaw_core::time::from_unix_secs(unix_secs).expect("valid unix seconds")
    }

    fn task_with(interval: Duration) -> ScheduledTask {
        ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(7)),
            SkillId::new(),
            json!({}),
            interval,
            "being",
        )
    }

    fn task_with_cron(cron: &str) -> ScheduledTask {
        task_with(Duration::seconds(120)).with_cron_expression(cron)
    }

    // (1) Puhdas erääntymislogiikka injektoidulla kellolla.
    #[test]
    fn fires_at_interval_not_before_and_not_again_until_next_window() {
        let interval = Duration::seconds(60);
        let start = at(0);

        // Ei koskaan laukennut → erääntynyt heti hetkellä 0.
        assert!(is_due(interval, None, start));

        // Laukesi hetkellä 0 → last_fired = 0.
        let last_fired = Some(at(0));

        // now = 30s: EI vielä erääntynyt (30 < 0 + 60).
        assert!(!is_due(interval, last_fired, at(30)));

        // now = 60s: erääntynyt (60 >= 0 + 60).
        assert!(is_due(interval, last_fired, at(60)));

        // Laukesi nyt 60s → last_fired = 60. now = 90s: EI uudelleen
        // (90 < 60 + 60). now = 120s: erääntynyt taas (120 >= 60 + 60).
        let last_fired = Some(at(60));
        assert!(!is_due(interval, last_fired, at(90)));
        assert!(is_due(interval, last_fired, at(120)));
    }

    #[test]
    fn never_fired_is_due_immediately() {
        let task = task_with(Duration::seconds(60));
        let decision = decide(&task, None, None, at(0));
        assert!(decision.due);
        assert!(decision.key.is_some());
    }

    #[test]
    fn disabled_task_is_never_due() {
        // Perhe-agency (Phase 4): kill-switch ohittaa erääntymisen. Sama tehtävä
        // joka muuten laukeaisi heti (ei koskaan laukennut) EI laukea kun
        // enabled=false.
        let task = task_with(Duration::seconds(60)).with_enabled(false);
        let decision = decide(&task, None, None, at(0));
        assert!(!decision.due, "disabloitu tehtävä ei laukea");
        assert!(decision.key.is_none(), "ei avainta kun ei laukea");

        // Uudelleen käyttöön → laukeaa taas.
        let reenabled = task.with_enabled(true);
        assert!(
            decide(&reenabled, None, None, at(0)).due,
            "käyttöön otto palauttaa"
        );
    }

    #[test]
    fn not_due_has_no_key() {
        let task = task_with(Duration::seconds(60));
        let decision = decide(&task, Some(at(0)), None, at(30));
        assert!(!decision.due);
        assert!(decision.key.is_none());
    }

    // (2) Deterministinen avain: sama looginen laukaisu → sama avain kahdella
    //     erillisellä arvioinnilla (kaatumis-restart-dedup outboxin läpi).
    #[test]
    fn same_logical_firing_yields_same_key_across_evaluations() {
        let task = task_with(Duration::seconds(60));

        // Kaksi eri now-arvoa SAMASSA intervalli-ikkunassa [60, 120):
        let eval_a = firing_key(task.id, task.interval, at(65));
        let eval_b = firing_key(task.id, task.interval, at(119));
        assert_eq!(eval_a, eval_b, "sama ikkuna → sama avain (restart-dedup)");

        // Seuraava ikkuna [120, 180) → eri avain (ei väärää deduplikointia).
        let next_window = firing_key(task.id, task.interval, at(120));
        assert_ne!(eval_a, next_window);
    }

    #[test]
    fn key_is_independent_of_process_memory() {
        // Avain johtuu vain task_id + interval + now, ei mistään tikki-tilasta.
        let task = task_with(Duration::seconds(30));
        let key1 = firing_key(task.id, task.interval, at(45));
        // "Restart": uusi arviointi samasta ikkunasta eri now-arvolla.
        let key2 = firing_key(task.id, task.interval, at(59));
        assert_eq!(key1, key2);
        assert!(key1.starts_with("schedule-"));
    }

    #[test]
    fn nonpositive_interval_is_always_due_with_stable_key() {
        assert!(is_due(Duration::zero(), Some(at(100)), at(100)));
        let task = task_with(Duration::zero());
        // Avain pysyy hyvin määriteltynä degeneroituneella intervallilla.
        let _ = firing_key(task.id, task.interval, at(7));
    }

    #[test]
    fn due_tasks_returns_only_due_in_order() {
        let a = ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(1)),
            SkillId::new(),
            json!({}),
            Duration::seconds(60),
            "b",
        );
        let b = ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(2)),
            SkillId::new(),
            json!({}),
            Duration::seconds(60),
            "b",
        );
        let tasks = vec![a.clone(), b.clone()];
        // a laukesi äsken (ei erääntynyt), b ei koskaan (erääntynyt).
        let due = due_tasks(
            &tasks,
            |id| {
                if id == a.id {
                    Some(at(50))
                } else {
                    None
                }
            },
            None,
            at(60),
        );
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].task_id, b.id);
    }

    // (Phase 4) vanhene-jos-ei-ihmistä: idle_expired + decide-integraatio.
    #[test]
    fn idle_expired_logic() {
        let idle = Some(Duration::seconds(100));
        // Ei idle-kattoa → ei koskaan vanhene.
        assert!(!idle_expired(None, None, at(1_000_000)));
        // Idle-katto + ei koskaan ihmistä → vanhentunut heti.
        assert!(idle_expired(idle, None, at(0)));
        // Ihminen aktiivinen äsken (50s sitten) → ei vanhentunut (50 < 100).
        assert!(!idle_expired(idle, Some(at(0)), at(50)));
        // Ihminen aktiivinen kauan sitten (150s) → vanhentunut (150 > 100).
        assert!(idle_expired(idle, Some(at(0)), at(150)));
        // Ei-positiivinen katto → ei vanhene (turvallinen degeneraatio).
        assert!(!idle_expired(Some(Duration::zero()), None, at(1_000_000)));
    }

    #[test]
    fn decide_respects_idle_expiry() {
        // Tehtävä joka muuten laukeaisi heti, mutta idle-katto + ei ihmistä →
        // ei laukea; ihmisaktiivisuuden myötä laukeaa taas.
        let task = task_with(Duration::seconds(60)).with_expire_after_idle(Duration::seconds(100));
        // Ei ihmistä → vanhentunut → ei laukea.
        assert!(!decide(&task, None, None, at(0)).due);
        // Ihminen aktiivinen nyt → laukeaa (ei vielä idleä).
        assert!(decide(&task, None, Some(at(0)), at(0)).due);
        // Ihminen 200s sitten → idle ylittyi → ei laukea.
        assert!(!decide(&task, None, Some(at(0)), at(200)).due);
    }

    #[test]
    fn parse_cron_accepts_standard_expression() {
        assert!(parse_cron("0 * * * *").is_some());
        assert!(parse_cron("not a cron").is_none());
    }

    #[test]
    fn cron_fires_on_schedule_not_before_occurrence() {
        // Joka tunti minuutilla 0 (UTC).
        let cron = "0 * * * *";
        let hour_start = at(3_600); // 01:00:00

        // Ei koskaan laukennut → erääntynyt ensimmäisellä esiintymällä.
        assert!(is_due_cron(cron, None, hour_start));

        // Laukesi 01:00 → ei uudelleen 01:30 (sama tunti-ikkuna).
        let last = Some(hour_start);
        assert!(!is_due_cron(cron, last, at(3_600 + 1_800)));

        // Seuraava tunnin alku 02:00 → erääntynyt taas.
        assert!(is_due_cron(cron, last, at(7_200)));
    }

    #[test]
    fn cron_firing_key_is_stable_within_occurrence() {
        let task = task_with_cron("0 * * * *");
        let key_a = cron_firing_key(task.id, "0 * * * *", at(3_650));
        let key_b = cron_firing_key(task.id, "0 * * * *", at(3_699));
        assert_eq!(key_a, key_b, "sama tunti-esiintymä → sama avain");
        assert!(key_a.starts_with("schedule-"));

        let next_hour = cron_firing_key(task.id, "0 * * * *", at(7_200));
        assert_ne!(key_a, next_hour, "eri esiintymä → eri avain");
    }

    #[test]
    fn decide_uses_cron_when_expression_set() {
        let task = task_with_cron("* * * * *");
        let decision = decide(&task, None, None, at(60));
        assert!(decision.due);
        let key = decision.key.expect("cron due has key");
        assert_eq!(key, cron_firing_key(task.id, "* * * * *", at(60)));

        // Intervalli 120s estäisi (90 < 0+120), mutta minuutticron laukeaa (viim. esiintymä 60s).
        let interval_task = task_with(Duration::seconds(120));
        assert!(!decide(&interval_task, Some(at(0)), None, at(90)).due);
        assert!(decide(&task, Some(at(0)), None, at(90)).due);
    }

    #[test]
    fn invalid_cron_expression_is_never_due() {
        let task = task_with(Duration::seconds(60)).with_cron_expression("not valid");
        assert!(!decide(&task, None, None, at(0)).due);
        assert!(decide(&task, None, None, at(0)).key.is_none());
    }
}
