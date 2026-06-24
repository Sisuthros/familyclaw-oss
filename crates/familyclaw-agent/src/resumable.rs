//! Jatkettavan vuoron tila ([`ResumableTurn`]) + sen kaatumiskestävä
//! tallennuspinta ([`ResumableTurnStore`]) — suspend/resume-sillan (roadmap §6)
//! pysyvä puoli.
//!
//! ## Mihin tätä tarvitaan
//! Kun [`Agent::think`](crate::Agent::think) ajaa tool-loopin ja työkalu vaatii
//! ihmisen hyväksynnän, vuoro **keskeytyy** ([`ThinkOutcome::Suspended`](crate::ThinkOutcome::Suspended)).
//! Hyväksyntä voi tulla minuutteja tai tunteja myöhemmin — mahdollisesti vasta
//! prosessin uudelleenkäynnistyksen jälkeen. Jotta vuoron voi **jatkaa siitä
//! mihin se jäi**, tool-loopin siihenastinen tila on tallennettava pysyvästi:
//! viestipino (LLM-konteksti), keskeyttäneen työkalukutsun tunniste ja
//! myönnetyn hyväksynnän tunniste. Tämä moduuli tallentaa juuri sen — eikä
//! mitään muuta.
//!
//! ## Salaisuusinvariantti (ehdoton)
//! Jatkettavaa vuoroa **ei koskaan** tallenneta raakojen salaisuuksien eikä
//! KERROS B -datan kanssa. [`ResumableTurn`] kantaa argumenteista vain
//! **SHA-256-tiivisteen** ([`ResumableTurn::arguments_hash`]) ja **redaktoidun
//! tiivistelmän** ([`ResumableTurn::redacted_arguments`]) — ei koskaan raakoja
//! työkaluargumentteja. Viestipino ([`ResumableTurn::messages`]) sisältää
//! tool-loopin LLM-kontekstin, joka on jo rakennettu redaktoiduista
//! todisteista (`familyclaw-actions` redaktoi todistepaketit ennen kuin niiden
//! teksti syötetään malliin) — kutsujan **vastuulla** on olla työntämättä
//! salaisuuksia viestipinoon, samoin kuin
//! [`PendingRecord::redacted_summary`](familyclaw_actions::PendingRecord)
//! :n kohdalla.
//!
//! Kenttä kentältä, miksi mikään kenttä ei vuoda salaisuutta — ks.
//! [`ResumableTurn`]:n dokumentaatio.
//!
//! ## Determinismi
//! Kaikki aikaa lukeva logiikka ottaa aikaleiman injektoituna
//! ([`familyclaw_core::time::Timestamp`]) — kelloa ei lueta tämän moduulin
//! sisällä. Vanhentuminen käyttää samaa fail-closed-rajaa kuin
//! [`familyclaw_actions::approval::Approval::is_expired`] (`now > expires_at`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use familyclaw_actions::approval::sha256_hex;
use familyclaw_actions::ApprovalId;
use familyclaw_bus::MessageOrigin;
use familyclaw_core::time::Timestamp;
use familyclaw_durable::{EntryKind, FileJournal, Journal, JournalEntry, StepId};

use crate::llm::LlmMessage;

/// Tämän moduulin oma virhetyyppi (tallennuspinnan I/O + sarjallistus).
///
/// Pidetään erillään [`familyclaw_core::FamilyClawError`]:sta, jotta
/// tallennuspinta pysyy ohuena ja itsenäisenä; [`crate::Agent`] kääräisee tämän
/// tarvittaessa ydintyyppiin.
#[derive(Debug)]
pub enum ResumableError {
    /// Journalin avaus, luku tai kirjoitus epäonnistui.
    Journal(String),
    /// [`ResumableTurn`]:n sarjallistus tai jäsennys epäonnistui.
    Serde(String),
    /// Pyydettyä jatkettavaa vuoroa ei löytynyt (tuntematon tunniste tai jo
    /// kulutettu/häädetty). **Fail-closed:** tuntematonta tunnistetta ei voi
    /// jatkaa.
    NotFound(ApprovalId),
    /// Jatkettava vuoro löytyi, mutta on **vanhentunut** (`now > expires_at`).
    /// Fail-closed: vanhentunutta vuoroa ei jatketa.
    Expired(ApprovalId),
}

impl std::fmt::Display for ResumableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResumableError::Journal(msg) => write!(f, "resumable journal error: {msg}"),
            ResumableError::Serde(msg) => write!(f, "resumable serde error: {msg}"),
            ResumableError::NotFound(id) => {
                write!(f, "resumable turn not found for approval {id}")
            }
            ResumableError::Expired(id) => {
                write!(f, "resumable turn expired for approval {id}")
            }
        }
    }
}

impl std::error::Error for ResumableError {}

/// Tämän moduulin tulostyyppi.
pub type Result<T> = std::result::Result<T, ResumableError>;

/// **Jatkettavan vuoron** salaisuudeton, pysyvä tila (roadmap §6 resumable-turn-state).
///
/// Tämä on tasan se tieto, jonka [`Agent::resume_approved`](crate::Agent::resume_approved)
/// tarvitsee jatkaakseen keskeytyneen tool-loopin siitä mihin se jäi — ei
/// enempää. Avaimena tallennuspinnalla toimii [`ResumableTurn::approval_id`].
///
/// ## Salaisuusinvariantti (kenttä kentältä)
/// Mikään kenttä ei kanna raakaa salaisuutta eikä KERROS B -dataa:
/// - [`approval_id`](Self::approval_id) — myönnetyn hyväksynnän tunniste (UUID,
///   ei salaisuus). Tallennuspinnan avain ja side `familyclaw-actions`:n
///   odottavaan hyväksyntään.
/// - [`being_id`](Self::being_id) — olennon bus-tunniste merkkijonona (UUID).
/// - [`conversation_origin`](Self::conversation_origin) — vastauksen kohde
///   (kanava/keskustelu/lähettäjä). Reititysmetatietoa, ei salaisuus.
/// - [`messages`](Self::messages) — tool-loopin LLM-viestipino. Sisältää
///   system-promptin, käyttäjän viestin ja siihenastiset työkalutulokset.
///   Työkalutulokset on johdettu **redaktoiduista todisteista**
///   (`familyclaw-actions` redaktoi ennen kuin teksti syötetään malliin), joten
///   ne eivät sisällä raakoja salaisuuksia. Kutsujan vastuulla on olla
///   työntämättä salaisuuksia tähän.
/// - [`tool_call_id`](Self::tool_call_id) — LLM:n antama työkalukutsun tunniste
///   (sitoo tulevan `tool_result`-viestin oikeaan kutsuun). Läpinäkyvä merkki.
/// - [`tool_name`](Self::tool_name) — keskeyttäneen työkalun nimi (manifestin
///   nimi, ei salaisuus).
/// - [`arguments_hash`](Self::arguments_hash) — työkaluargumenttien
///   SHA-256-**tiiviste** (ei raakoja argumentteja). Sitoo jatkettavan vuoron
///   tarkasti niihin argumentteihin, joille hyväksyntä myönnettiin.
/// - [`redacted_arguments`](Self::redacted_arguments) — ihmisluettava,
///   redaktoitu tiivistelmä siitä mitä työkalu tekisi. **Ei raakoja
///   argumentteja, ei salaisuuksia.**
/// - [`created_at`](Self::created_at) / [`expires_at`](Self::expires_at) —
///   aikaleimat (auditointi + TTL).
/// - [`policy_snapshot`](Self::policy_snapshot) — käytäntö-tilannekuva
///   keskeytyshetkellä (esim. vaadittu oikeus). Neutraalia metatietoa.
/// - [`audit_ids`](Self::audit_ids) — viittaukset jo kirjattuihin
///   audit-tapahtumiin (UUID:t), jotta resume voi linkittää itsensä
///   keskeytyksen audit-jälkeen.
/// - [`turn_id`](Self::turn_id) / [`durable_cursor`](Self::durable_cursor) —
///   vuoron järjestysnumero + durable-lokin kursoripaikka keskeytyshetkellä.
///   Diagnostiikkaa ja resumea varten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResumableTurn {
    /// Myönnetyn hyväksynnän tunniste (tallennuspinnan avain).
    pub approval_id: ApprovalId,
    /// Vuoron suorittaneen olennon bus-tunniste merkkijonona.
    pub being_id: String,
    /// Vastauksen kohde (kanava/keskustelu/lähettäjä) jatkamista varten;
    /// `None` jos vuorolla ei ollut per-viesti-alkuperää (staattinen kohde).
    pub conversation_origin: Option<MessageOrigin>,
    /// Tool-loopin LLM-viestipino keskeytyshetkellä (system + user +
    /// siihenastiset assistant/tool-viestit). Tästä loop jatkaa.
    pub messages: Vec<LlmMessage>,
    /// Keskeyttäneen työkalukutsun LLM-tunniste (`tool_result` sitoutuu tähän).
    pub tool_call_id: String,
    /// Keskeyttäneen työkalun nimi (manifestin nimi).
    pub tool_name: String,
    /// Työkaluargumenttien SHA-256-tiiviste (EI raakoja argumentteja).
    pub arguments_hash: String,
    /// Redaktoitu, ihmisluettava tiivistelmä työkalun argumenteista/toimesta.
    pub redacted_arguments: String,
    /// Keskeytyksen luontihetki (auditointi).
    pub created_at: Timestamp,
    /// Hetki jonka jälkeen jatkettava vuoro on vanhentunut (= hyväksynnän TTL).
    pub expires_at: Timestamp,
    /// Käytäntö-tilannekuva keskeytyshetkellä (neutraali metatieto).
    pub policy_snapshot: String,
    /// Viittaukset keskeytyksen audit-tapahtumiin (UUID-merkkijonoja).
    pub audit_ids: Vec<String>,
    /// Vuoron järjestysnumero olennon elinkaaressa keskeytyshetkellä.
    pub turn_id: u64,
    /// Durable-lokin kursoripaikka keskeytyshetkellä (diagnostiikka).
    pub durable_cursor: u64,
}

impl ResumableTurn {
    /// Rakentaa jatkettavan vuoron tilan **tiivistäen argumentit**: raakoja
    /// argumentteja ei oteta vastaan, vaan kutsuja antaa jo tiivisteen ja
    /// redaktoidun tiivistelmän. Näin tyyppiä on käytännössä mahdotonta
    /// rakentaa salaisuuden kanssa.
    ///
    /// `tool_arguments` on raaka JSON, josta lasketaan **vain** SHA-256-tiiviste
    /// — itse arvoa ei tallenneta. Tämä on payload-sidonnan vastine: kun resume
    /// myöhemmin jatkaa, hyväksyntä kulutetaan samaa tiivistettä vasten.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        approval_id: ApprovalId,
        being_id: impl Into<String>,
        conversation_origin: Option<MessageOrigin>,
        messages: Vec<LlmMessage>,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        tool_arguments: &serde_json::Value,
        redacted_arguments: impl Into<String>,
        created_at: Timestamp,
        expires_at: Timestamp,
    ) -> Self {
        // Argumenteista TALLENNETAAN VAIN TIIVISTE. Sarjallistus epäonnistuu
        // käytännössä koskaan (Value→Vec<u8>); jos se epäonnistuu, tiiviste
        // lasketaan tyhjästä — se vain estää resumen (mismatch), ei vuoda mitään.
        let raw = serde_json::to_vec(tool_arguments).unwrap_or_default();
        let arguments_hash = sha256_hex(&raw);
        Self {
            approval_id,
            being_id: being_id.into(),
            conversation_origin,
            messages,
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            arguments_hash,
            redacted_arguments: redacted_arguments.into(),
            created_at,
            expires_at,
            policy_snapshot: String::new(),
            audit_ids: Vec::new(),
            turn_id: 0,
            durable_cursor: 0,
        }
    }

    /// Liittää käytäntö-tilannekuvan (ketjutus). Neutraali metatieto, ei salaisuus.
    #[must_use]
    pub fn with_policy_snapshot(mut self, snapshot: impl Into<String>) -> Self {
        self.policy_snapshot = snapshot.into();
        self
    }

    /// Liittää audit-tapahtumien tunnisteet (ketjutus).
    #[must_use]
    pub fn with_audit_ids(mut self, ids: Vec<String>) -> Self {
        self.audit_ids = ids;
        self
    }

    /// Liittää durable-paikan: vuoron numero + kursoripaikka (ketjutus).
    #[must_use]
    pub const fn with_durable_position(mut self, turn_id: u64, durable_cursor: u64) -> Self {
        self.turn_id = turn_id;
        self.durable_cursor = durable_cursor;
        self
    }

    /// Onko jatkettava vuoro vanhentunut hetkeen `now` nähden (`now > expires_at`).
    ///
    /// Sama fail-closed-raja kuin [`familyclaw_actions::approval::Approval::is_expired`]:
    /// tasan `expires_at` kelpaa vielä, aidosti myöhempi ei.
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        now > self.expires_at
    }
}

/// **Jatkettavien vuorojen tallennuspinta.**
///
/// Abstrahoi sen, missä jatkettavat vuorot elävät — muistissa vai
/// kaatumiskestävällä levyllä. Sama sopimus kuin
/// [`familyclaw_actions::PendingApprovalStore`]:lla: kaikki metodit ovat
/// `&self` (sisäinen mutaatio lukon takana), jotta trait on `dyn`-yhteensopiva.
///
/// ## Sopimus
/// - [`put`](Self::put) tallentaa jatkettavan vuoron avaimella
///   `turn.approval_id`. Saman avaimen uudelleenkirjoitus korvaa aiemman.
/// - [`get`](Self::get) palauttaa tallennetun vuoron, `None` jos ei löydy.
/// - [`remove`](Self::remove) kuluttaa (poistaa) vuoron kertakäyttöisesti.
/// - [`evict_expired`](Self::evict_expired) häätää vanhentuneet fail-closed-rajalla.
///
/// ## Salaisuudet
/// Levylle tallentava toteutus saa kirjoittaa vain [`ResumableTurn`]:n
/// salaisuudettomat kentät (tiiviste + tunnisteet + redaktoidut tiivistelmät +
/// redaktoiduista todisteista johdettu viestipino) — ei koskaan raakoja
/// argumentteja eikä salaisuuksia.
pub trait ResumableTurnStore: Send + Sync {
    /// Tallentaa (tai korvaa) jatkettavan vuoron avaimella `turn.approval_id`.
    ///
    /// # Errors
    /// [`ResumableError::Journal`]/[`ResumableError::Serde`] jos levytoteutuksen
    /// kirjoitus tai sarjallistus epäonnistuu.
    fn put(&self, turn: ResumableTurn) -> Result<()>;

    /// Hakee jatkettavan vuoron hyväksynnän tunnisteella; `None` jos ei löydy.
    ///
    /// # Errors
    /// [`ResumableError::Journal`]/[`ResumableError::Serde`] levytoteutuksilla.
    fn get(&self, approval_id: ApprovalId) -> Result<Option<ResumableTurn>>;

    /// Poistaa (kuluttaa) jatkettavan vuoron ja palauttaa sen, jos se oli
    /// olemassa; `None` jos ei. Kertakäyttöinen: poiston jälkeen ei enää löydy.
    ///
    /// # Errors
    /// [`ResumableError::Journal`]/[`ResumableError::Serde`] levytoteutuksilla.
    fn remove(&self, approval_id: ApprovalId) -> Result<Option<ResumableTurn>>;

    /// Jatkettavien vuorojen lukumäärä.
    ///
    /// # Errors
    /// [`ResumableError::Journal`]/[`ResumableError::Serde`] levytoteutuksilla.
    fn len(&self) -> Result<usize>;

    /// Onko pinta tyhjä.
    ///
    /// # Errors
    /// Sama kuin [`len`](Self::len).
    fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Poistaa kaikki hetkeen `now` mennessä vanhentuneet vuorot ja palauttaa
    /// häädettyjen lukumäärän. Sama fail-closed-raja kuin
    /// [`ResumableTurn::is_expired`].
    ///
    /// # Errors
    /// [`ResumableError::Journal`]/[`ResumableError::Serde`] levytoteutuksilla.
    fn evict_expired(&self, now: Timestamp) -> Result<usize>;
}

/// Muistinvarainen tallennuspinta ([`HashMap`] traitin takana).
///
/// Oletus ja testikäyttö: nopea, **mutta ei selviä kaatumisesta**. Tuotannossa,
/// jossa resume-kaatumiskestävyys on vaatimus, käytä [`JournalResumableStore`]:a.
#[derive(Debug, Default)]
pub struct InMemoryResumableStore {
    /// Hyväksynnän tunniste → jatkettava vuoro.
    inner: Mutex<HashMap<ApprovalId, ResumableTurn>>,
}

impl InMemoryResumableStore {
    /// Luo tyhjän muistipinnan.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lukitsee kartan, toipuen myrkytetystä lukosta paniikkaamatta.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ApprovalId, ResumableTurn>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ResumableTurnStore for InMemoryResumableStore {
    fn put(&self, turn: ResumableTurn) -> Result<()> {
        self.lock().insert(turn.approval_id, turn);
        Ok(())
    }

    fn get(&self, approval_id: ApprovalId) -> Result<Option<ResumableTurn>> {
        Ok(self.lock().get(&approval_id).cloned())
    }

    fn remove(&self, approval_id: ApprovalId) -> Result<Option<ResumableTurn>> {
        Ok(self.lock().remove(&approval_id))
    }

    fn len(&self) -> Result<usize> {
        Ok(self.lock().len())
    }

    fn evict_expired(&self, now: Timestamp) -> Result<usize> {
        let mut map = self.lock();
        let before = map.len();
        map.retain(|_, turn| !turn.is_expired(now));
        Ok(before - map.len())
    }
}

/// Journal-rivin looginen nimi jatkettavan vuoron tallennukselle.
const RESUMABLE_PUT: &str = "resumable_turn_put";
/// Journal-rivin looginen nimi jatkettavan vuoron poistolle (tombstone).
const RESUMABLE_DELETE: &str = "resumable_turn_delete";

/// Tiivistyksen oletuskerroin: loki tiivistetään automaattisesti kun fyysisten
/// rivien määrä ylittää `AUTO_COMPACT_FACTOR * elävien_vuorojen_määrä`.
///
/// Kerroin 2 = "tiivistä kun vähintään puolet riveistä on kuolleita". Rajaa
/// kuolleiden rivien kertymisen vakiokertoimeen elävää kohti, joten lokin koko
/// ja replayn O(n)-kustannus pysyvät elävän tilan kokoluokassa.
const AUTO_COMPACT_FACTOR: usize = 2;

/// Pienin fyysinen rivimäärä jolla auto-tiivistys ylipäänsä harkitaan (estää
/// turhan tiivistyksen pienillä lokeilla).
const AUTO_COMPACT_MIN_ROWS: usize = 64;

/// Kaatumiskestävä tallennuspinta [`FileJournal`]:n päällä.
///
/// Append-only-loki: jokainen tallennus kirjoitetaan `resumable_turn_put`-
/// markerina (koko salaisuudeton [`ResumableTurn`]) ja jokainen poisto
/// `resumable_turn_delete`-markerina (vain hyväksynnän tunniste, tombstone).
/// Tila rekonstruoidaan toistamalla loki: myöhempi rivi voittaa, joten poisto
/// kumoaa lisäyksen.
///
/// Koska [`FileJournal::append`] flushaa ja fsyncaa ennen paluuta, valmistunut
/// tallennus on levyllä myös äkillisen kaatumisen jälkeen — **jatkettava vuoro
/// selviää keskeytyksen ja resumen välisestä kaatumisesta**, joten hyväksynnän
/// myöntämisen jälkeen vuoron voi jatkaa loppuun vaikka prosessi olisi
/// käynnistynyt välissä uudelleen.
///
/// ## Tiivistys (compaction) — rajaton kasvu kuriin
/// Koska loki on append-only, jokainen poisto ([`remove`](ResumableTurnStore::remove)
/// / [`evict_expired`](ResumableTurnStore::evict_expired)) ja saman tunnisteen
/// korvaus jättää **kuolleita rivejä** lokiin: tila on oikea (myöhempi rivi
/// voittaa), mutta tiedosto kasvaa rajatta ja replay muuttuu O(n):ksi
/// rivimäärässä. [`compact`](JournalResumableStore::compact) kirjoittaa lokin
/// uudelleen sisältämään **vain elävät vuorot** atomisesti
/// [`FileJournal::rewrite`]:n kautta (temp + fsync + rename) — elävä tila säilyy
/// bitilleen eikä keskeytyminen menetä eläviä vuoroja. Tiivistys laukeaa joko
/// operaattorin kutsumana tai **automaattisesti** tallennuksen ja häädön
/// yhteydessä kun kuolleiden rivien osuus ylittää kynnyksen (ks.
/// `AUTO_COMPACT_FACTOR` ja [`with_auto_compact_factor`](JournalResumableStore::with_auto_compact_factor)).
///
/// ## Salaisuusinvariantti
/// Levylle kirjoitetaan vain [`ResumableTurn`]:n salaisuudettomat kentät (ks.
/// [`ResumableTurn`]) — ei koskaan raakoja työkaluargumentteja eikä salaisuuksia.
/// Tiivistys säilyttää tämän: uudelleenkirjoitettu loki sisältää samat
/// salaisuudettomat `resumable_turn_put`-rivit.
pub struct JournalResumableStore {
    /// Append-only-loki johon tallennukset ja poistot kirjataan.
    journal: FileJournal,
    /// Seuraavan rivin sekvenssipaikka (monotoninen).
    next_step: Mutex<StepId>,
    /// Auto-tiivistyksen kerroin: tiivistä kun `rivit > factor * elävät`.
    /// `0` poistaa auto-tiivistyksen käytöstä (vain manuaalinen `compact`).
    auto_compact_factor: usize,
}

impl std::fmt::Debug for JournalResumableStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalResumableStore")
            .field("path", &self.journal.path())
            .finish_non_exhaustive()
    }
}

impl JournalResumableStore {
    /// Avaa (tai luo) kaatumiskestävän pinnan annetusta tiedostopolusta.
    ///
    /// Olemassa olevasta lokista jatkettavat vuorot rekonstruoidaan heti, joten
    /// uudelleenkäynnistyksen jälkeen ne ovat yhä [`get`](ResumableTurnStore::get)-
    /// haettavissa ja jatkettavissa.
    ///
    /// # Errors
    /// [`ResumableError::Journal`] jos journalia ei voi avata tai lukea.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let journal = FileJournal::open(path)
            .map_err(|e| ResumableError::Journal(format!("open resumable journal failed: {e}")))?;
        let len = journal
            .len()
            .map_err(|e| ResumableError::Journal(format!("read resumable journal failed: {e}")))?;
        let next = StepId::new(u64::try_from(len).unwrap_or(u64::MAX));
        Ok(Self {
            journal,
            next_step: Mutex::new(next),
            auto_compact_factor: AUTO_COMPACT_FACTOR,
        })
    }

    /// Asettaa auto-tiivistyksen kertoimen (ketjutus).
    ///
    /// Loki tiivistetään automaattisesti kun fyysisten rivien määrä ylittää
    /// `factor * elävien_vuorojen_määrä` (ja rivejä on vähintään
    /// `AUTO_COMPACT_MIN_ROWS`). Oletus on `AUTO_COMPACT_FACTOR` (2). Arvo `0`
    /// **poistaa** auto-tiivistyksen käytöstä — loki tiivistetään vain
    /// [`compact`](Self::compact)-kutsulla.
    #[must_use]
    pub const fn with_auto_compact_factor(mut self, factor: usize) -> Self {
        self.auto_compact_factor = factor;
        self
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

    /// Liittää markerin lokiin annetulla nimellä ja hyötykuormalla.
    fn append_marker(&self, name: &str, payload: serde_json::Value) -> Result<()> {
        let entry = JournalEntry::marker(self.next_step_id(), name, payload);
        self.journal
            .append(entry)
            .map_err(|e| ResumableError::Journal(format!("append resumable marker failed: {e}")))
    }

    /// Rekonstruoi nykytilan toistamalla lokin (myöhempi rivi voittaa).
    fn replay_state(&self) -> Result<HashMap<ApprovalId, ResumableTurn>> {
        let entries = self.journal.replay_all().map_err(|e| {
            ResumableError::Journal(format!("replay resumable journal failed: {e}"))
        })?;
        Self::reconstruct_state(entries)
    }

    /// Rakentaa nykytilan annetuista journal-riveistä (puhdas funktio, ei I/O).
    ///
    /// Toisto käy rivit järjestyksessä: `resumable_turn_put` lisää/korvaa vuoron,
    /// `resumable_turn_delete` poistaa sen (tombstone). Myöhempi rivi voittaa.
    /// Eriytetty [`replay_state`](Self::replay_state):stä jotta sekä levyltä lukeva
    /// replay että [`compact`](Self::compact):n [`FileJournal::compact_with`]-suljin
    /// rakentavat tilan **samalla logiikalla** — jälkimmäinen saa rivit valmiiksi
    /// luettuina lukon alta, eikä saa lukea journalia uudelleen (deadlock).
    fn reconstruct_state(entries: Vec<JournalEntry>) -> Result<HashMap<ApprovalId, ResumableTurn>> {
        let mut state: HashMap<ApprovalId, ResumableTurn> = HashMap::new();
        for entry in entries {
            let EntryKind::Marker { name, payload } = entry.kind else {
                continue;
            };
            match name.as_str() {
                RESUMABLE_PUT => {
                    let turn: ResumableTurn = serde_json::from_value(payload).map_err(|e| {
                        ResumableError::Serde(format!("decode resumable put failed: {e}"))
                    })?;
                    state.insert(turn.approval_id, turn);
                }
                RESUMABLE_DELETE => {
                    let id: ApprovalId = serde_json::from_value(payload).map_err(|e| {
                        ResumableError::Serde(format!("decode resumable delete id failed: {e}"))
                    })?;
                    state.remove(&id);
                }
                _ => {}
            }
        }
        Ok(state)
    }

    /// Fyysisten journal-rivien määrä (eläviä + kuolleita). Dead-row-suhteen
    /// mittauspohja; eroaa [`len`](ResumableTurnStore::len):stä joka palauttaa
    /// vain elävien vuorojen määrän.
    fn physical_row_count(&self) -> Result<usize> {
        self.journal
            .len()
            .map_err(|e| ResumableError::Journal(format!("read resumable journal len failed: {e}")))
    }

    /// Kirjoittaa lokin uudelleen sisältämään **vain elävät vuorot** (tiivistys),
    /// pudottaen kaikki kuolleet rivit (tombstonet ja korvatut `put`-rivit).
    /// Palauttaa pudotettujen kuolleiden rivien määrän.
    ///
    /// Elävä tila säilyy bitilleen: tiivistyksen jälkeen täsmälleen samat vuorot
    /// ovat [`get`](ResumableTurnStore::get)-haettavissa, ja uudelleenlatauksesta
    /// (restart) rekonstruoituu identtinen tila. Tiivistys on **atominen**
    /// ([`FileJournal::rewrite`]: temp + fsync + rename) — jos prosessi kaatuu
    /// kesken, elävä tiedosto on yhä ehjässä vanhassa tilassaan eikä yhtään
    /// elävää vuoroa katoa.
    ///
    /// Rivit uudelleennumeroidaan tiiviiksi `0..N`-sekvenssiksi ja sisäinen
    /// sekvenssikursori asetetaan vastaamaan, jotta tulevat tallennukset jatkavat
    /// oikealta paikalta.
    ///
    /// # Errors
    /// [`ResumableError::Serde`] jos jonkin vuoron sarjallistus epäonnistuu;
    /// [`ResumableError::Journal`] jos lokin luku tai atominen uudelleenkirjoitus
    /// epäonnistuu. Virhetilanteessa elävä loki jätetään entiselleen.
    pub fn compact(&self) -> Result<usize> {
        // Atominen tiivistys appendeja vastaan: [`FileJournal::compact_with`]
        // pitää saman file-lukon koko luku→suodatus→swap-operaation ajan, joten
        // rinnakkainen tallennus/poisto ei voi laskeutua aukkoon ja kadota
        // (TOCTOU-korjaus). `build`-suljin saa luetut rivit, rekonstruoi elävän
        // tilan ja palauttaa uudelleennumeroidut elävät RESUMABLE_PUT-rivit.
        //
        // Elävien rivien määrä smugletaan sulkimesta `Cell`:llä, jotta
        // sekvenssikursori voidaan asettaa swapin jälkeen (suljin EI saa lukita
        // journalia uudelleen → ei voi lukea kursoria omalta polultaan).
        let live_count = std::cell::Cell::new(0usize);
        let dropped = self
            .journal
            .compact_with(|entries| {
                // Rekonstruoi elävä tila valmiiksi luetuista riveistä (sama
                // logiikka kuin replayssa, mutta EI uudelleenlukua — uudelleenluku
                // lukitsisi journalin ja deadlockkaisi). ResumableError kääritään
                // DurableError-tekstiksi jotta tyyppi sopii compact_with-sopimukseen.
                let state = Self::reconstruct_state(entries).map_err(|e| {
                    familyclaw_durable::DurableError::step_failed(
                        "compact_reconstruct",
                        e.to_string(),
                    )
                })?;
                // Yksi RESUMABLE_PUT-rivi per elävä vuoro, uudelleennumeroituna 0..N.
                let mut kept = Vec::with_capacity(state.len());
                let mut step = StepId::ZERO;
                for turn in state.values() {
                    let payload = serde_json::to_value(turn)?;
                    kept.push(JournalEntry::marker(step, RESUMABLE_PUT, payload));
                    step = step.next();
                }
                live_count.set(kept.len());
                Ok(kept)
            })
            .map_err(|e| {
                ResumableError::Journal(format!("compact resumable journal failed: {e}"))
            })?;

        // Sekvenssikursori osoittamaan tiivistetyn lokin perään (= elävien määrä,
        // koska rivit uudelleennumeroitiin tiiviisti 0..N).
        {
            let mut guard = self
                .next_step
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = StepId::new(u64::try_from(live_count.get()).unwrap_or(u64::MAX));
        }

        Ok(dropped)
    }

    /// Tiivistää lokin **jos** kuolleiden rivien osuus ylittää kynnyksen.
    ///
    /// Laukaisuehto: `auto_compact_factor > 0` JA fyysisiä rivejä on vähintään
    /// [`AUTO_COMPACT_MIN_ROWS`] JA `rivit > factor * elävät`. Kutsutaan
    /// tallennuksen ja häädön jälkeen. Auto-tiivistyksen epäonnistuminen **ei**
    /// kaada kutsujaa: data on jo turvallisesti lokissa, joten tiivistys on pelkkä
    /// optimointi — virhe niellään (loki vain pysyy tiivistämättömänä tällä
    /// kertaa).
    fn maybe_auto_compact(&self) {
        if self.auto_compact_factor == 0 {
            return;
        }
        let Ok(rows) = self.physical_row_count() else {
            return;
        };
        if rows < AUTO_COMPACT_MIN_ROWS {
            return;
        }
        let Ok(live) = self.replay_state().map(|s| s.len()) else {
            return;
        };
        if rows > self.auto_compact_factor.saturating_mul(live) {
            let _ = self.compact();
        }
    }
}

impl ResumableTurnStore for JournalResumableStore {
    fn put(&self, turn: ResumableTurn) -> Result<()> {
        let payload = serde_json::to_value(&turn)
            .map_err(|e| ResumableError::Serde(format!("encode resumable turn failed: {e}")))?;
        self.append_marker(RESUMABLE_PUT, payload)?;
        // Korvaus jätti kuolleen rivin (vanha put) → harkitse auto-tiivistystä.
        self.maybe_auto_compact();
        Ok(())
    }

    fn get(&self, approval_id: ApprovalId) -> Result<Option<ResumableTurn>> {
        Ok(self.replay_state()?.remove(&approval_id))
    }

    fn remove(&self, approval_id: ApprovalId) -> Result<Option<ResumableTurn>> {
        let existing = self.replay_state()?.remove(&approval_id);
        if existing.is_some() {
            // Tombstone vain jos vuoro oli olemassa — turha rivi vältetään.
            let payload = serde_json::to_value(approval_id).map_err(|e| {
                ResumableError::Serde(format!("encode resumable delete id failed: {e}"))
            })?;
            self.append_marker(RESUMABLE_DELETE, payload)?;
            // Tombstone on kuollut rivi → harkitse auto-tiivistystä.
            self.maybe_auto_compact();
        }
        Ok(existing)
    }

    fn len(&self) -> Result<usize> {
        Ok(self.replay_state()?.len())
    }

    fn evict_expired(&self, now: Timestamp) -> Result<usize> {
        let state = self.replay_state()?;
        let expired: Vec<ApprovalId> = state
            .values()
            .filter(|turn| turn.is_expired(now))
            .map(|turn| turn.approval_id)
            .collect();
        for id in &expired {
            let payload = serde_json::to_value(id).map_err(|e| {
                ResumableError::Serde(format!("encode resumable delete id failed: {e}"))
            })?;
            self.append_marker(RESUMABLE_DELETE, payload)?;
        }
        if !expired.is_empty() {
            // Häätö tuotti tombstoneja (kuolleita rivejä) → harkitse tiivistystä.
            self.maybe_auto_compact();
        }
        Ok(expired.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use familyclaw_core::time::from_unix_secs;
    use std::path::PathBuf;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    /// Apuri: jatkettava vuoro annetulla TTL:llä ja argumenteilla.
    fn turn_at(now: Timestamp, ttl: Duration, args: &serde_json::Value) -> ResumableTurn {
        ResumableTurn::new(
            ApprovalId::new(),
            BeingIdStr(),
            Some(MessageOrigin::new("discord-main", "conv-1", "user-9")),
            vec![
                LlmMessage::system("you are a generic being"),
                LlmMessage::user("draft a github issue"),
            ],
            "call_abc123",
            "github_issue_draft",
            args,
            "github_issue_draft({title: <redacted>})",
            now,
            now + ttl,
        )
    }

    /// Geneerinen being-id-merkkijono testeihin (ei salaisuus).
    #[allow(non_snake_case)]
    fn BeingIdStr() -> String {
        "00000000-0000-4000-8000-000000000001".to_string()
    }

    /// RAII-temp-tiedosto ilman ulkoisia crateja.
    struct TempPath(PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "familyclaw-resumable-{tag}-{}-{:?}.jsonl",
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

    #[test]
    fn in_memory_put_get_remove_roundtrip() {
        let store = InMemoryResumableStore::new();
        let now = at(1_700_000_000);
        let args = serde_json::json!({ "title": "Login broken" });
        let turn = turn_at(now, Duration::minutes(60), &args);
        let id = turn.approval_id;

        store.put(turn).expect("put");
        assert_eq!(store.len().expect("len"), 1);

        let got = store.get(id).expect("get").expect("present");
        assert_eq!(got.approval_id, id);
        assert_eq!(got.tool_name, "github_issue_draft");
        // Payload-sidonta: tiiviste vastaa samaa argument-arvoa.
        assert_eq!(
            got.arguments_hash,
            sha256_hex(&serde_json::to_vec(&args).unwrap())
        );

        let removed = store.remove(id).expect("remove").expect("present");
        assert_eq!(removed.approval_id, id);
        assert!(store.get(id).expect("get").is_none());
        assert!(store.is_empty().expect("empty"));
    }

    #[test]
    fn arguments_are_hashed_not_stored_raw() {
        // Argumentti sisältää salaisuuden — vain tiiviste tallennetaan.
        let now = at(1_700_000_000);
        let secret = format!("sk-{}", "live".repeat(4));
        let args = serde_json::json!({ "api_key": secret });
        let turn = turn_at(now, Duration::minutes(60), &args);
        // Sarjallistettu muoto ei sisällä raakaa salaisuutta.
        let json = serde_json::to_string(&turn).expect("serialize");
        assert!(
            !json.contains(&secret),
            "raw secret must never be in the turn"
        );
        // Mutta tiiviste on läsnä.
        assert!(json.contains(&sha256_hex(&serde_json::to_vec(&args).unwrap())));
    }

    #[test]
    fn ttl_eviction_drops_expired_only() {
        let store = InMemoryResumableStore::new();
        let now = at(1_700_000_000);
        let args = serde_json::json!({ "x": 1 });
        let short = turn_at(now, Duration::seconds(60), &args);
        let long = turn_at(now, Duration::seconds(3600), &args);
        let short_id = short.approval_id;
        let long_id = long.approval_id;
        store.put(short).expect("put short");
        store.put(long).expect("put long");

        let evicted = store.evict_expired(at(1_700_000_120)).expect("evict");
        assert_eq!(evicted, 1);
        assert!(store.get(short_id).expect("get").is_none());
        assert!(store.get(long_id).expect("get").is_some());
    }

    #[test]
    fn ttl_boundary_keeps_exactly_at_expiry() {
        let store = InMemoryResumableStore::new();
        let now = at(1_700_000_000);
        let args = serde_json::json!({ "x": 1 });
        let turn = turn_at(now, Duration::seconds(60), &args);
        let id = turn.approval_id;
        store.put(turn).expect("put");

        // Tasan expires_at EI vanhentunut (sama fail-closed-raja).
        assert_eq!(store.evict_expired(at(1_700_000_060)).expect("evict"), 0);
        assert!(store.get(id).expect("get").is_some());
        assert_eq!(store.evict_expired(at(1_700_000_061)).expect("evict"), 1);
        assert!(store.get(id).expect("get").is_none());
    }

    #[test]
    fn durable_reloads_after_simulated_restart() {
        let tmp = TempPath::new("reload");
        let now = at(1_700_000_000);
        let args = serde_json::json!({ "title": "Bug" });
        let turn = turn_at(now, Duration::minutes(60), &args);
        let id = turn.approval_id;
        let hash = turn.arguments_hash.clone();

        // Vaihe 1: tallenna ja PUDOTA (simuloi kaatuminen).
        {
            let store = JournalResumableStore::open(tmp.path()).expect("open 1");
            store.put(turn).expect("put");
            assert_eq!(store.len().expect("len"), 1);
        }

        // Vaihe 2: rakenna pinta UUDELLEEN samasta tiedostosta — vuoro säilyi.
        let resumed = JournalResumableStore::open(tmp.path()).expect("open 2");
        assert_eq!(resumed.len().expect("len"), 1, "resumable survived restart");
        let got = resumed.get(id).expect("get").expect("still present");
        assert_eq!(got.approval_id, id);
        assert_eq!(got.arguments_hash, hash);
        assert_eq!(got.messages.len(), 2, "message stack survived");

        // Kulutus säilyy tombstonena yli vielä yhden restartin.
        resumed.remove(id).expect("remove").expect("present");
        let after = JournalResumableStore::open(tmp.path()).expect("open 3");
        assert!(after.get(id).expect("get").is_none());
        assert!(after.is_empty().expect("empty"));
    }

    #[test]
    fn durable_persisted_form_contains_no_raw_secret() {
        let tmp = TempPath::new("no-secret");
        let now = at(1_700_000_000);
        let secret = format!("sk-{}", "live".repeat(4));
        let args = serde_json::json!({ "api_key": secret.clone() });
        let turn = turn_at(now, Duration::minutes(60), &args);

        let store = JournalResumableStore::open(tmp.path()).expect("open");
        store.put(turn).expect("put");

        let on_disk = std::fs::read_to_string(tmp.path()).expect("read journal");
        assert!(
            !on_disk.contains(&secret),
            "persisted resumable turn must never contain the raw secret"
        );
        // Tiiviste ON läsnä (sidonta säilyy).
        assert!(on_disk.contains(&sha256_hex(&serde_json::to_vec(&args).unwrap())));
    }

    #[test]
    fn get_unknown_is_none() {
        let store = InMemoryResumableStore::new();
        assert!(store.get(ApprovalId::new()).expect("get").is_none());
    }

    // ---- Compaction ----

    /// Laskee fyysiset (eläviä + kuolleita) journal-rivit lukemalla tiedoston.
    fn physical_rows(path: &Path) -> usize {
        std::fs::read_to_string(path)
            .map_or(0, |s| s.lines().filter(|l| !l.trim().is_empty()).count())
    }

    /// Apuri: tallenna `n` vuoroa, palauta niiden tunnisteet.
    fn put_n(store: &JournalResumableStore, now: Timestamp, n: usize) -> Vec<ApprovalId> {
        let args = serde_json::json!({ "x": 1 });
        let mut ids = Vec::with_capacity(n);
        for _ in 0..n {
            let turn = turn_at(now, Duration::minutes(60), &args);
            ids.push(turn.approval_id);
            store.put(turn).expect("put");
        }
        ids
    }

    #[test]
    fn compact_drops_dead_rows_keeps_live_turns() {
        let tmp = TempPath::new("compact-basic");
        let now = at(1_700_000_000);
        let store = JournalResumableStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        let ids = put_n(&store, now, 10);
        for id in ids.iter().take(5) {
            store.remove(*id).expect("remove");
        }
        assert_eq!(physical_rows(tmp.path()), 15, "10 put + 5 tombstone");
        assert_eq!(store.len().expect("len"), 5);

        let dropped = store.compact().expect("compact");
        assert_eq!(dropped, 10, "15 rows → 5 live = 10 dropped");
        assert_eq!(physical_rows(tmp.path()), 5);
        assert_eq!(store.len().expect("len"), 5);

        for id in ids.iter().take(5) {
            assert!(store.get(*id).expect("get").is_none(), "removed gone");
        }
        for id in ids.iter().skip(5) {
            assert!(store.get(*id).expect("get").is_some(), "live present");
        }
    }

    #[test]
    fn compact_preserves_exact_state_across_reload() {
        let tmp = TempPath::new("compact-reload");
        let now = at(1_700_000_000);

        let ids = {
            let store = JournalResumableStore::open(tmp.path())
                .expect("open 1")
                .with_auto_compact_factor(0);
            let ids = put_n(&store, now, 6);
            for id in ids.iter().take(3) {
                store.remove(*id).expect("remove");
            }
            store.compact().expect("compact");
            ids
        };

        // Restart pelkästä tiivistetystä tiedostosta → identtinen tila.
        let resumed = JournalResumableStore::open(tmp.path()).expect("open 2");
        assert_eq!(resumed.len().expect("len"), 3);
        for id in ids.iter().take(3) {
            assert!(
                resumed.get(*id).expect("get").is_none(),
                "removed stay removed"
            );
        }
        for id in ids.iter().skip(3) {
            assert!(resumed.get(*id).expect("get").is_some(), "live stay live");
        }
        assert_eq!(physical_rows(tmp.path()), 3);
    }

    #[test]
    fn compact_is_atomic_temp_then_rename() {
        let tmp = TempPath::new("compact-atomic");
        let now = at(1_700_000_000);
        let store = JournalResumableStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        let ids = put_n(&store, now, 8);
        for id in &ids {
            store.remove(*id).expect("remove");
        }
        let live = put_n(&store, now, 1);
        store.compact().expect("compact");

        // Jokainen levyllä oleva rivi jäsentyy ehjäksi (ei puolikasta renamesta).
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read");
        for line in on_disk.lines().filter(|l| !l.trim().is_empty()) {
            serde_json::from_str::<JournalEntry>(line).expect("intact json line");
        }
        assert_eq!(store.len().expect("len"), 1);
        assert!(store.get(live[0]).expect("get").is_some());

        // Ei orpoa temp-tiedostoa tämän lokin nimellä.
        let dir = tmp.path().parent().expect("parent");
        let own = tmp
            .path()
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        let leftover: Vec<_> = std::fs::read_dir(dir)
            .expect("read_dir")
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&own) && n.contains(".compact-") && n.contains(".tmp"))
            .collect();
        assert!(leftover.is_empty(), "no temp left: {leftover:?}");
    }

    #[test]
    fn compact_drops_expired_turns() {
        let tmp = TempPath::new("compact-expired");
        let now = at(1_700_000_000);
        let store = JournalResumableStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        let args = serde_json::json!({ "x": 1 });
        let short = turn_at(now, Duration::seconds(60), &args);
        let short_id = short.approval_id;
        let long = turn_at(now, Duration::seconds(3600), &args);
        let long_id = long.approval_id;
        store.put(short).expect("put short");
        store.put(long).expect("put long");

        assert_eq!(store.evict_expired(at(1_700_000_120)).expect("evict"), 1);
        store.compact().expect("compact");

        assert!(store.get(short_id).expect("get").is_none());
        assert!(store.get(long_id).expect("get").is_some());
        assert_eq!(physical_rows(tmp.path()), 1, "only the live turn remains");
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read");
        assert!(
            !on_disk.contains(&short_id.to_string()),
            "expired id gone from disk"
        );
    }

    #[test]
    fn auto_compact_triggers_when_dead_rows_exceed_threshold() {
        let tmp = TempPath::new("auto-compact");
        let now = at(1_700_000_000);
        let store = JournalResumableStore::open(tmp.path()).expect("open"); // default factor
        let args = serde_json::json!({ "x": 1 });

        // Yksi pysyvä elävä.
        let keeper = turn_at(now, Duration::minutes(60), &args);
        let keeper_id = keeper.approval_id;
        store.put(keeper).expect("put keeper");

        // 100 put+remove paria → ilman tiivistystä 201 riviä.
        for _ in 0..100 {
            let t = turn_at(now, Duration::minutes(60), &args);
            let id = t.approval_id;
            store.put(t).expect("put churn");
            store.remove(id).expect("remove churn");
        }

        let rows = physical_rows(tmp.path());
        assert!(
            rows < 50,
            "auto-compaction should keep log small, got {rows}"
        );
        assert!(store.get(keeper_id).expect("get").is_some());
        assert_eq!(store.len().expect("len"), 1);
    }

    #[test]
    fn compact_on_empty_log_is_noop() {
        let tmp = TempPath::new("compact-empty");
        let store = JournalResumableStore::open(tmp.path()).expect("open");
        assert_eq!(store.compact().expect("compact"), 0);
        assert!(store.is_empty().expect("empty"));
        assert_eq!(physical_rows(tmp.path()), 0);
    }

    /// TOCTOU-aukon sulkemisen regressio: tiivistys lukee tilan ja kirjoittaa
    /// lokin uudelleen **saman file-lukon alla** ([`FileJournal::compact_with`]),
    /// joten rinnakkainen tallennus ei voi laskeutua aukkoon ja kadota. Tässä ei
    /// aja oikeaa rinnakkaisuutta (race on epädeterministinen) — todistetaan
    /// rakenteesta seuraava havaittava invariantti: tiivistyksen PALUUN JÄLKEEN
    /// tehty tallennus laskeutuu tiivistettyjen elävien vuorojen PERÄÄN, ja
    /// uudelleenlataus tuottaa täsmälleen oikean tilan. Concurrent-append-
    /// turvallisuus seuraa nyt yhden-lukon-pidosta.
    #[test]
    fn compact_then_put_does_not_lose_post_compact_turn() {
        let tmp = TempPath::new("compact-toctou");
        let now = at(1_700_000_000);
        let store = JournalResumableStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        let ids = put_n(&store, now, 6);
        for id in ids.iter().take(3) {
            store.remove(*id).expect("remove");
        }
        assert_eq!(store.len().expect("len"), 3, "3 live before compact");

        let dropped = store.compact().expect("compact");
        assert_eq!(
            dropped, 6,
            "9 rows (6 put + 3 tombstone) → 3 live = 6 dropped"
        );
        assert_eq!(physical_rows(tmp.path()), 3, "only live rows after compact");

        // Tiivistyksen JÄLKEEN tallennettu vuoro laskeutuu elävien PERÄÄN.
        let args = serde_json::json!({ "x": 2 });
        let post = turn_at(now, Duration::minutes(60), &args);
        let post_id = post.approval_id;
        store.put(post).expect("put after compact");
        assert_eq!(store.len().expect("len"), 4, "3 live + 1 post-compact");
        assert!(store.get(post_id).expect("get").is_some());

        // Uudelleenlataus tuottaa täsmälleen oikean tilan.
        let resumed = JournalResumableStore::open(tmp.path()).expect("reopen");
        assert_eq!(resumed.len().expect("len"), 4);
        for id in ids.iter().take(3) {
            assert!(
                resumed.get(*id).expect("get").is_none(),
                "removed stay removed"
            );
        }
        for id in ids.iter().skip(3) {
            assert!(resumed.get(*id).expect("get").is_some(), "live stay live");
        }
        assert!(
            resumed.get(post_id).expect("get").is_some(),
            "post-compact survives reload"
        );
    }
}
