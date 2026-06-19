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
//! `now >= last_fired + interval`. Jos tehtävä ei ole koskaan laukennut
//! (`last_fired = None`), se on erääntynyt heti ensimmäisellä arvioinnilla.
//! Ei-positiivinen intervalli kohdellaan "aina erääntyneenä" (turvallinen
//! degeneraatio; tuotannossa intervallin oletetaan olevan positiivinen).
//!
//! ## Idempotenssiavaimen vakaus
//! Laukaisun avain on `schedule-{task_id}-{epoch_bucket}`, jossa
//! `epoch_bucket = floor(now_unix / interval_secs)`. Saman intervalli-ikkunan
//! sisällä **mikä tahansa** `now` tuottaa saman `epoch_bucket`-arvon ja siten
//! saman avaimen. Näin sama looginen laukaisu → sama avain, myös jos prosessi
//! kaatuu ja arvioi saman ikkunan uudelleen restartin jälkeen
//! ([`firing_key`]).

use chrono::Duration;
use familyclaw_core::time::Timestamp;

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
pub fn decide(task: &ScheduledTask, last_fired: Option<Timestamp>, now: Timestamp) -> DueDecision {
    let due = is_due(task.interval, last_fired, now);
    let key = if due {
        Some(firing_key(task.id, task.interval, now))
    } else {
        None
    };
    DueDecision {
        task_id: task.id,
        due,
        key,
    }
}

/// Laskee erääntyneet tehtävät listalle kerralla.
///
/// `last_fired` on hakulausekefunktio joka palauttaa tehtävän viimeisen
/// laukaisuajan (tai `None` jos ei koskaan laukennut). Palauttaa vain
/// erääntyneiden tehtävien päätökset, syötteen järjestyksessä (deterministinen).
#[must_use]
pub fn due_tasks<F>(
    tasks: &[ScheduledTask],
    mut last_fired: F,
    now: Timestamp,
) -> Vec<DueDecision>
where
    F: FnMut(ScheduledTaskId) -> Option<Timestamp>,
{
    tasks
        .iter()
        .map(|task| decide(task, last_fired(task.id), now))
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
        let decision = decide(&task, None, at(0));
        assert!(decision.due);
        assert!(decision.key.is_some());
    }

    #[test]
    fn not_due_has_no_key() {
        let task = task_with(Duration::seconds(60));
        let decision = decide(&task, Some(at(0)), at(30));
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
            at(60),
        );
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].task_id, b.id);
    }
}
