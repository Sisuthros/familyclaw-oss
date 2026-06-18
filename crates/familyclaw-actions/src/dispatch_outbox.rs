//! Lähetyksen idempotenssi-outbox ([`DispatchOutboxStore`]) — KERROS A.
//!
//! ## Ongelma jonka tämä ratkaisee (exactly-once-rajan kivijalka)
//! [`crate::facade::ActionRuntime::submit_task`] suorittaa **ulkoisen
//! sivuvaikutuksen** (putken ajo, todisteen tallennus, odottavan hyväksynnän
//! kirjaus). Agenttikerros kääräisee tämän durable-askeleeseen, mutta
//! sivuvaikutuksen **suoritus** ja askeleen **journalointi** ovat kaksi erillistä
//! tapahtumaa: niiden VÄLISSÄ on ikkuna. Jos prosessi tapetaan (SIGKILL) juuri
//! siinä — sivuvaikutus on jo tapahtunut mutta journal-riviä ei ole — replay ei
//! näe riviä, luulee askelta ajamattomaksi ja **ajaa `submit_task`:n uudelleen**.
//! Tulos: kaksoislaukaisu (double-fire), joka rikkoo "exactly-once side effects
//! under SIGKILL" -väitteen.
//!
//! Pelkkä journaloinnin siirtäminen sivuvaikutuksen **eteen** ei korjaa tätä:
//! silloin voisi journaloida sivuvaikutuksen joka ei koskaan tapahtunut (kaatui
//! ennen suoritusta) → väärä "exactly-once" toiseen suuntaan.
//!
//! ## Ratkaisu: idempotenssi-avain ajoympäristön RAJALLA
//! Outbox kytkee jokaiseen lähetykseen **vakaan idempotenssi-avaimen** (kutsuja
//! johtaa sen deterministisesti, esim. `turn-{turn}-dispatch-{k}`). Lähetys
//! kirjataan kaksivaiheisesti **kaatumiskestävään** lokiin:
//!
//! 1. **intent** (`DISPATCH_INTENT`) kirjataan **ENNEN** sivuvaikutusta.
//! 2. sivuvaikutus suoritetaan.
//! 3. **committed** (`DISPATCH_COMMITTED`) kirjataan sivuvaikutuksen jälkeen,
//!    sisältäen lähetyksen lopputuloksen ([`DispatchedOutcome`]).
//!
//! Kun sama avain nähdään uudelleen (replay tai restart):
//! - **committed** löytyy → palautetaan tallennettu lopputulos **arvo-identtisenä**
//!   (sama `task_id` / `ApprovalId` / TTL) **ajamatta sivuvaikutusta uudelleen**.
//! - **intent mutta ei committed** → prosessi kaatui kesken sivuvaikutuksen.
//!   Sivuvaikutus on voinut tapahtua osittain → palautusperiaate on **eksplisiittinen
//!   ja fail-closed** ([`DispatchLookup::InProgress`]): kutsua EI ajeta uudelleen,
//!   vaan se hylätään, ettei sokeasti kahdenneta.
//! - **ei mitään** → avainta ei ole koskaan aloitettu → sivuvaikutus on
//!   turvallista suorittaa.
//!
//! ## Takuun tarkka raja (rehellisesti)
//! - **Prosessin kaatuminen / SIGKILL:** taattu. Sivuvaikutus suoritetaan
//!   korkeintaan kerran; committed-tilan saavuttanut lähetys palautuu identtisenä
//!   eikä koskaan ajeta uudelleen.
//! - **Power-loss / hakemiston metadata-fsync:** [`crate::pending_store::JournalPendingStore`]:n
//!   tavoin tämä nojaa [`familyclaw_durable::FileJournal`]:n `flush` + `fsync`-takuuseen
//!   *tiedoston* sisällölle. Hakemiston merkinnän (dir-fsync) ja laitteiston
//!   kirjoituspuskureiden osalta takuu on yhtä vahva kuin alla oleva FS/laitteisto —
//!   tätä **ei yliluvata**. Intent-only-jälki kaatumisen jälkeen on aina
//!   havaittavissa, ja palautusperiaate sen varalle on eksplisiittinen.
//!
//! ## Salaisuusinvariantti
//! Tallennettu muoto ([`DispatchedOutcome`]) sisältää vain tunnisteet, tilan ja
//! mahdollisen hyväksynnän tunnisteen + virheviestin — **ei raakaa payloadia eikä
//! salaisuuksia**. Sama invariantti kuin [`crate::pending_store::PendingRecord`]:lla.
//!
//! ## Determinismi
//! Aikaa ei lueta moduulin sisällä; idempotenssi-avain annetaan kutsujalta.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use familyclaw_durable::{EntryKind, FileJournal, Journal, JournalEntry, StepId};

use crate::error::{ActionError, Result};
use crate::facade::SubmitOutcome;
use crate::ids::{ActionTaskId, ApprovalId};
use crate::task::TaskStatus;

/// Moduulin valmiusaste — säilytetään, jotta [`crate::all_modules_scaffolded`]
/// kääntyy edelleen muiden moduulien rinnalla.
pub(crate) const SCAFFOLDED: bool = true;

/// Journal-rivin looginen nimi lähetyksen **aikeelle** (kirjataan ENNEN sivuvaikutusta).
const DISPATCH_INTENT: &str = "dispatch_intent";
/// Journal-rivin looginen nimi lähetyksen **sitoutumiselle** (kirjataan sivuvaikutuksen jälkeen).
const DISPATCH_COMMITTED: &str = "dispatch_committed";

/// Lähetyksen journaloitava, **salaisuudeton** lopputulos.
///
/// Tämä on [`SubmitOutcome`]:n tallennusmuoto outboxissa: tasan se osa jonka
/// kutsuja tarvitsee jatkaakseen — `task_id`, `status` ja mahdollinen
/// `pending_approval` — sekä mahdollinen `submit_task`:n virheviesti, jotta
/// myös epäonnistunut lähetys palautuu samana ajamatta sivuvaikutusta uudelleen.
///
/// Ei sisällä raakaa payloadia eikä salaisuuksia.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchedOutcome {
    /// Lähetetyn tehtävän tunniste.
    pub task_id: ActionTaskId,
    /// Tehtävän tila lähetyksen jälkeen.
    pub status: TaskStatus,
    /// Hyväksynnän tunniste jos lähetys jäi odottamaan hyväksyntää (muuten `None`).
    pub pending_approval: Option<ApprovalId>,
    /// `submit_task`:n virheviesti jos lähetys epäonnistui (muuten `None`).
    pub error: Option<String>,
}

impl DispatchedOutcome {
    /// Rakentaa journaloitavan lopputuloksen onnistuneesta lähetyksestä.
    #[must_use]
    pub const fn from_submit(outcome: &SubmitOutcome) -> Self {
        Self {
            task_id: outcome.task_id,
            status: outcome.status,
            pending_approval: outcome.pending_approval,
            error: None,
        }
    }

    /// Rakentaa journaloitavan lopputuloksen epäonnistuneesta lähetyksestä.
    ///
    /// Tallentaa virheviestin (nil-tunniste + [`TaskStatus::Failed`]), jotta
    /// replay palauttaa saman virheen ajamatta sivuvaikutusta uudelleen.
    #[must_use]
    pub fn from_error(message: impl Into<String>) -> Self {
        Self {
            task_id: ActionTaskId::nil(),
            status: TaskStatus::Failed,
            pending_approval: None,
            error: Some(message.into()),
        }
    }

    /// Palauttaa tämän lopputuloksen [`Result<SubmitOutcome>`]-muodossa.
    ///
    /// Jos tallennettu lopputulos kantoi virheen, palautetaan
    /// [`ActionError::ExecutionFailed`] samalla viestillä; muuten onnistunut
    /// [`SubmitOutcome`] arvo-identtisenä alkuperäisen lähetyksen kanssa.
    ///
    /// # Errors
    /// [`ActionError::ExecutionFailed`] jos tallennettu lähetys oli virhe.
    pub fn into_result(self) -> Result<SubmitOutcome> {
        if let Some(message) = self.error {
            return Err(ActionError::ExecutionFailed(message));
        }
        Ok(SubmitOutcome {
            task_id: self.task_id,
            status: self.status,
            pending_approval: self.pending_approval,
        })
    }
}

/// Yhden idempotenssi-avaimen tila outboxissa.
///
/// Palautetaan [`DispatchOutboxStore::lookup`]:sta jotta kutsuja tietää, onko
/// sivuvaikutus turvallista suorittaa, jo suoritettu vai kesken kaatunut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchLookup {
    /// Avainta ei ole koskaan aloitettu → sivuvaikutus on turvallista suorittaa.
    NotStarted,
    /// Lähetys on jo sitoutunut → palautettava lopputulos ajamatta uudelleen.
    Committed(DispatchedOutcome),
    /// Aie kirjattu mutta ei sitoutumista → prosessi kaatui kesken sivuvaikutuksen.
    /// Palautusperiaate: fail-closed (älä aja uudelleen).
    InProgress,
}

/// Yhden avaimen sisäinen rekonstruoitu tila (intent nähty? committed nähty?).
#[derive(Debug, Clone, Default)]
struct KeyState {
    /// Onko avaimelle kirjattu aie (`DISPATCH_INTENT`).
    intent: bool,
    /// Sitoutunut lopputulos, jos `DISPATCH_COMMITTED` on kirjattu.
    committed: Option<DispatchedOutcome>,
}

/// Kaatumiskestävä lähetyksen idempotenssi-outbox.
///
/// Trait jotta julkisivu ([`crate::facade::ActionRuntime`]) voi käyttää joko
/// muistinvaraista ([`InMemoryDispatchOutbox`], oletus, ei selviä kaatumisesta)
/// tai kaatumiskestävää ([`JournalDispatchOutbox`]) toteutusta vaihtamatta
/// logiikkaansa. Kaikki metodit ovat `&self` (sisäinen mutaatio lukon takana),
/// jotta trait on `dyn`-yhteensopiva.
pub trait DispatchOutboxStore: std::fmt::Debug + Send + Sync {
    /// Tarkistaa avaimen nykytilan **suorittamatta** mitään.
    ///
    /// # Errors
    /// Levytoteutuksilla [`ActionError::Proof`] jos lokin luku epäonnistuu.
    fn lookup(&self, key: &str) -> Result<DispatchLookup>;

    /// Kirjaa avaimen **aikeen** (`DISPATCH_INTENT`) — kutsuttava **ENNEN**
    /// sivuvaikutuksen suoritusta.
    ///
    /// # Errors
    /// Levytoteutuksilla [`ActionError::Proof`] jos kirjoitus epäonnistuu.
    fn record_intent(&self, key: &str) -> Result<()>;

    /// Kirjaa avaimen **sitoutumisen** (`DISPATCH_COMMITTED`) lopputuloksineen —
    /// kutsuttava **vasta** sivuvaikutuksen onnistuneen suorituksen jälkeen.
    ///
    /// # Errors
    /// Levytoteutuksilla [`ActionError::Proof`] jos kirjoitus epäonnistuu.
    fn record_committed(&self, key: &str, outcome: &DispatchedOutcome) -> Result<()>;
}

/// Muistinvarainen outbox (oletus + testikäyttö).
///
/// Nopea, mutta **ei selviä prosessin kaatumisesta** — uudelleenkäynnistyksessä
/// tila on tyhjä. Tämä on tarkoituksellisesti sama käyttäytyminen kuin ennen
/// outboxia: in-memory-ajoympäristö ei tarjoa exactly-once-takuuta kaatumisen
/// yli (käytä [`JournalDispatchOutbox`]:a tuotannossa).
#[derive(Debug, Default)]
pub struct InMemoryDispatchOutbox {
    /// Avain → tila.
    inner: Mutex<HashMap<String, KeyState>>,
}

impl InMemoryDispatchOutbox {
    /// Luo tyhjän muistinvaraisen outboxin.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lukitsee sisäisen kartan, toipuen myrkytetystä lukosta paniikkaamatta.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, KeyState>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl DispatchOutboxStore for InMemoryDispatchOutbox {
    fn lookup(&self, key: &str) -> Result<DispatchLookup> {
        let map = self.lock();
        Ok(match map.get(key) {
            None => DispatchLookup::NotStarted,
            Some(state) => match &state.committed {
                Some(outcome) => DispatchLookup::Committed(outcome.clone()),
                None if state.intent => DispatchLookup::InProgress,
                None => DispatchLookup::NotStarted,
            },
        })
    }

    fn record_intent(&self, key: &str) -> Result<()> {
        self.lock().entry(key.to_string()).or_default().intent = true;
        Ok(())
    }

    fn record_committed(&self, key: &str, outcome: &DispatchedOutcome) -> Result<()> {
        let mut map = self.lock();
        let state = map.entry(key.to_string()).or_default();
        state.intent = true;
        state.committed = Some(outcome.clone());
        Ok(())
    }
}

/// Outboxin yksi tallennusrivi (intent tai committed) levymuodossa.
///
/// Pieni salaisuudeton tietue: avain + valinnainen lopputulos (vain
/// committed-riveillä).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OutboxRow {
    /// Idempotenssi-avain.
    key: String,
    /// Lopputulos (vain committed-riveillä; intent-riveillä `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<DispatchedOutcome>,
}

/// Kaatumiskestävä outbox [`FileJournal`]:n päällä.
///
/// Append-only-loki: `dispatch_intent`- ja `dispatch_committed`-markerit, joista
/// tila rekonstruoidaan toistamalla. Koska [`FileJournal::append`] flushaa ja
/// fsyncaa ennen paluuta, kirjattu intent/committed on levyllä myös äkillisen
/// kaatumisen jälkeen — tämä on koko exactly-once-takuun kivijalka.
///
/// ## Salaisuusinvariantti
/// Levylle kirjoitetaan vain `OutboxRow`:n salaisuudettomat kentät (avain +
/// tunnisteet + tila). Ei raakaa payloadia eikä salaisuuksia.
pub struct JournalDispatchOutbox {
    /// Append-only-loki johon aikeet ja sitoutumiset kirjataan.
    journal: FileJournal,
    /// Seuraavan rivin sekvenssipaikka (monotoninen).
    next_step: Mutex<StepId>,
}

impl std::fmt::Debug for JournalDispatchOutbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalDispatchOutbox")
            .field("path", &self.journal.path())
            .finish_non_exhaustive()
    }
}

impl JournalDispatchOutbox {
    /// Avaa (tai luo) kaatumiskestävän outboxin annetusta tiedostopolusta.
    ///
    /// Olemassa olevasta lokista avainten tila rekonstruoituu heti, joten
    /// uudelleenkäynnistyksen jälkeen jo sitoutuneet lähetykset palautuvat
    /// identtisinä ja kesken jääneet havaitaan ([`DispatchLookup::InProgress`]).
    ///
    /// # Errors
    /// [`ActionError::Proof`] jos journalia ei voi avata tai lukea.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let journal = FileJournal::open(path)
            .map_err(|e| ActionError::Proof(format!("open dispatch outbox failed: {e}")))?;
        let len = journal
            .len()
            .map_err(|e| ActionError::Proof(format!("read dispatch outbox failed: {e}")))?;
        let next = StepId::new(u64::try_from(len).unwrap_or(u64::MAX));
        Ok(Self {
            journal,
            next_step: Mutex::new(next),
        })
    }

    /// Palauttaa lokin tiedostopolun.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.journal.path()
    }

    /// Varaa ja palauttaa seuraavan sekvenssipaikan (monotoninen).
    fn next_step_id(&self) -> StepId {
        let mut guard = self
            .next_step
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = *guard;
        *guard = current.next();
        current
    }

    /// Liittää markerin lokiin annetulla nimellä ja rivillä.
    fn append_marker(&self, name: &str, row: &OutboxRow) -> Result<()> {
        let payload = serde_json::to_value(row)
            .map_err(|e| ActionError::Proof(format!("encode outbox row failed: {e}")))?;
        let entry = JournalEntry::marker(self.next_step_id(), name, payload);
        self.journal
            .append(entry)
            .map_err(|e| ActionError::Proof(format!("append outbox marker failed: {e}")))
    }

    /// Rekonstruoi yhden avaimen tilan toistamalla lokin.
    ///
    /// Toisto käy rivit järjestyksessä: `dispatch_intent` merkitsee aikeen,
    /// `dispatch_committed` tallentaa lopputuloksen. Myöhempi committed voittaa.
    fn replay_key(&self, key: &str) -> Result<KeyState> {
        let entries = self
            .journal
            .replay_all()
            .map_err(|e| ActionError::Proof(format!("replay dispatch outbox failed: {e}")))?;
        let mut state = KeyState::default();
        for entry in entries {
            let EntryKind::Marker { name, payload } = entry.kind else {
                continue;
            };
            let is_intent = name == DISPATCH_INTENT;
            let is_committed = name == DISPATCH_COMMITTED;
            if !is_intent && !is_committed {
                continue;
            }
            let row: OutboxRow = serde_json::from_value(payload)
                .map_err(|e| ActionError::Proof(format!("decode outbox row failed: {e}")))?;
            if row.key != key {
                continue;
            }
            if is_intent {
                state.intent = true;
            } else if let Some(outcome) = row.outcome {
                state.intent = true;
                state.committed = Some(outcome);
            }
        }
        Ok(state)
    }
}

impl DispatchOutboxStore for JournalDispatchOutbox {
    fn lookup(&self, key: &str) -> Result<DispatchLookup> {
        let state = self.replay_key(key)?;
        Ok(match state.committed {
            Some(outcome) => DispatchLookup::Committed(outcome),
            None if state.intent => DispatchLookup::InProgress,
            None => DispatchLookup::NotStarted,
        })
    }

    fn record_intent(&self, key: &str) -> Result<()> {
        self.append_marker(
            DISPATCH_INTENT,
            &OutboxRow {
                key: key.to_string(),
                outcome: None,
            },
        )
    }

    fn record_committed(&self, key: &str, outcome: &DispatchedOutcome) -> Result<()> {
        self.append_marker(
            DISPATCH_COMMITTED,
            &OutboxRow {
                key: key.to_string(),
                outcome: Some(outcome.clone()),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// RAII-temp-tiedosto ilman ulkoisia crateja.
    struct TempPath(PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "familyclaw-outbox-{tag}-{}-{:?}.jsonl",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ));
            let _ = std::fs::remove_file(&p);
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn sample_outcome() -> DispatchedOutcome {
        DispatchedOutcome {
            task_id: ActionTaskId::new(),
            status: TaskStatus::NeedsApproval,
            pending_approval: Some(ApprovalId::new()),
            error: None,
        }
    }

    #[test]
    fn in_memory_lookup_lifecycle() {
        let outbox = InMemoryDispatchOutbox::new();
        assert_eq!(outbox.lookup("k").expect("lookup"), DispatchLookup::NotStarted);

        outbox.record_intent("k").expect("intent");
        assert_eq!(outbox.lookup("k").expect("lookup"), DispatchLookup::InProgress);

        let outcome = sample_outcome();
        outbox.record_committed("k", &outcome).expect("commit");
        match outbox.lookup("k").expect("lookup") {
            DispatchLookup::Committed(got) => assert_eq!(got, outcome),
            other => panic!("expected Committed, got {other:?}"),
        }
    }

    #[test]
    fn outcome_roundtrips_through_result() {
        let outcome = sample_outcome();
        let result = outcome.clone().into_result().expect("ok");
        assert_eq!(result.task_id, outcome.task_id);
        assert_eq!(result.pending_approval, outcome.pending_approval);

        let err_outcome = DispatchedOutcome::from_error("boom");
        let err = err_outcome.into_result().expect_err("err");
        assert!(matches!(err, ActionError::ExecutionFailed(_)));
    }

    #[test]
    fn durable_committed_survives_simulated_restart() {
        let tmp = TempPath::new("commit-survives");
        let outcome = sample_outcome();

        // Vaihe 1: kirjaa intent + committed, sitten "kaadu" (drop).
        {
            let outbox = JournalDispatchOutbox::open(tmp.path()).expect("open 1");
            outbox.record_intent("turn-0-dispatch-0").expect("intent");
            outbox
                .record_committed("turn-0-dispatch-0", &outcome)
                .expect("commit");
        }

        // Vaihe 2: avaa UUDELLEEN — committed-lopputulos palautuu identtisenä.
        let resumed = JournalDispatchOutbox::open(tmp.path()).expect("open 2");
        match resumed.lookup("turn-0-dispatch-0").expect("lookup") {
            DispatchLookup::Committed(got) => assert_eq!(got, outcome),
            other => panic!("expected Committed after restart, got {other:?}"),
        }
    }

    #[test]
    fn durable_intent_only_is_in_progress_after_restart() {
        let tmp = TempPath::new("intent-only");

        // Vaihe 1: kirjaa VAIN intent (simuloi kaatuminen kesken sivuvaikutuksen).
        {
            let outbox = JournalDispatchOutbox::open(tmp.path()).expect("open 1");
            outbox.record_intent("turn-0-dispatch-0").expect("intent");
        }

        // Vaihe 2: avaa uudelleen — intent-only → InProgress (fail-closed).
        let resumed = JournalDispatchOutbox::open(tmp.path()).expect("open 2");
        assert_eq!(
            resumed.lookup("turn-0-dispatch-0").expect("lookup"),
            DispatchLookup::InProgress,
            "intent ilman committed → InProgress kaatumisen jälkeen"
        );
        // Tuntematon avain on yhä NotStarted.
        assert_eq!(
            resumed.lookup("turn-0-dispatch-9").expect("lookup"),
            DispatchLookup::NotStarted
        );
    }

    #[test]
    fn durable_persisted_form_contains_no_raw_secret() {
        let tmp = TempPath::new("no-secret");
        // Avain on kutsujan johtama, ei salaisuus; varmistetaan silti ettei
        // outcome-tallennus vuoda mitään avainten/tunnisteiden ulkopuolelta.
        let outbox = JournalDispatchOutbox::open(tmp.path()).expect("open");
        outbox.record_intent("turn-3-dispatch-2").expect("intent");
        outbox
            .record_committed("turn-3-dispatch-2", &sample_outcome())
            .expect("commit");
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read");
        assert!(on_disk.contains("turn-3-dispatch-2"));
        assert!(!on_disk.contains("sk-"));
        assert!(!on_disk.contains("Bearer "));
    }

    #[test]
    fn separate_keys_are_independent() {
        let outbox = InMemoryDispatchOutbox::new();
        outbox.record_intent("a").expect("a intent");
        outbox.record_committed("a", &sample_outcome()).expect("a commit");
        // Eri avain on koskematon.
        assert_eq!(outbox.lookup("b").expect("lookup"), DispatchLookup::NotStarted);
    }
}
