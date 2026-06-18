//! Odottavien hyväksyntöjen tallennuspinta ([`PendingApprovalStore`]) — KERROS A.
//!
//! [`crate::facade::ActionRuntime`] jättää `write-external`-tehtävän odottamaan
//! ihmisen hyväksyntää: `submit-task` myöntää payload-sidotun hyväksynnän ja
//! tallentaa odottavan kirjauksen, jonka `approve` myöhemmin kuluttaa. Aiemmin
//! tämä kirjaus eli pelkässä prosessin sisäisessä `HashMap`:ssa — **prosessin
//! kaatuminen `submit-task`:n ja `approve`:n välissä menetti odottavan
//! hyväksynnän pysyvästi**, jolloin jo myönnetty toiminto jäi roikkumaan eikä
//! sitä voinut enää hyväksyä eikä evätä.
//!
//! Tämä moduuli abstrahoi tallennuksen [`PendingApprovalStore`]-traitin taakse
//! ja tarjoaa kaksi toteutusta:
//!
//! - [`InMemoryPendingStore`] — `HashMap` traitin takana. Oletus + testikäyttö.
//!   Nopea, mutta **ei** selviä kaatumisesta.
//! - [`JournalPendingStore`] — kaatumiskestävä, [`familyclaw_durable::FileJournal`]-
//!   pohjainen append-only-loki. Jokainen lisäys ja poisto kirjataan levylle
//!   (flush + fsync), ja uudelleenkäynnistyksessä tila rekonstruoidaan lokista —
//!   odottava hyväksyntä **säilyy kaatumisen yli** ja on yhä hyväksyttävissä.
//!
//! ## Salaisuusinvariantti (ehdoton)
//! Tallennettu muoto ([`PendingRecord`]) ei koskaan sisällä **raakaa payloadia,
//! salaisuuksia eikä KERROS B -dataa** — vain:
//! - hyväksynnän ja tehtävän tunnisteet,
//! - payloadin SHA-256-**tiivisteen** ([`crate::approval::Approval::payload_hash`]),
//! - redaktoidun ihmisluettavan tiivistelmän ([`PendingRecord::redacted_summary`]),
//! - luonti- ja vanhentumisaikaleimat.
//!
//! Payload-sidonta säilyy tiivisteen kautta: kun `approve` myöhemmin kuluttaa
//! hyväksynnän, esitetty payload tiivistetään uudelleen ja verrataan
//! tallennettuun tiivisteeseen ([`crate::approval::ApprovalLedger::consume`]).
//! Levyltä ei siis koskaan voi lukea itse payloadia takaisin.
//!
//! ## Kapasiteettikatto, TTL-häätö ja rate-limit-koukku
//! - **Kapasiteettikatto** ([`PendingCapacity`]): lisäys hylätään fail-closed
//!   ([`ActionError::PolicyDenied`]) kun odottavia on jo katon verran — estää
//!   muistin/levyn rajattoman kasvun (DoS-suoja).
//! - **TTL-häätö** ([`PendingApprovalStore::evict_expired`]): vanhentuneet
//!   kirjaukset poistetaan käyttäen täsmälleen samaa fail-closed-vanhentumista
//!   kuin [`crate::approval`] (`now > expires_at`). Vanhentunutta hyväksyntää ei
//!   voi enää kuluttaa, joten sen säilyttäminen olisi pelkkää roskaa.
//! - **Rate-limit-koukku** ([`DangerousToolRateLimiter`]): per-olento-laskuri
//!   vaarallisten (hyväksyntää vaativien) työkalukutsujen rajoittamiseen
//!   liukuvalla aikaikkunalla. Koukku on valinnainen ja deterministinen
//!   (aikaleima injektoidaan).
//!
//! ## Determinismi
//! Kaikki aikaa lukeva logiikka ottaa aikaleiman injektoituna
//! ([`familyclaw_core::time::Timestamp`]) — kelloa ei lueta moduulin sisällä.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use familyclaw_core::time::Timestamp;
use familyclaw_durable::{EntryKind, FileJournal, Journal, JournalEntry, StepId};

use crate::approval::Approval;
use crate::error::{ActionError, Result};
use crate::ids::{ActionTaskId, ApprovalId};

/// Moduulin valmiusaste — säilytetään, jotta [`crate::all_modules_scaffolded`]
/// kääntyy edelleen muiden moduulien rinnalla.
pub(crate) const SCAFFOLDED: bool = true;

/// Odottavan hyväksynnän **salaisuudeton** tallennusmuoto.
///
/// Tämä on yksi rivi tallennuspinnalla: se kantaa juuri sen tiedon jonka
/// `approve` tarvitsee jatkaakseen pysähtyneen tehtävän suoritusta — tehtävän
/// tunnisteen ja payload-sidotun hyväksynnän — sekä redaktoidun tiivistelmän
/// operaattorin näytettäväksi.
///
/// ## Salaisuusinvariantti
/// Kenttä kentältä:
/// - [`PendingRecord::task_id`] — tehtävän UUID (ei salaisuus).
/// - [`PendingRecord::approval`] — [`Approval`], jonka ainoa payload-johdannainen
///   kenttä on SHA-256-**tiiviste** (ei raaka payload). Loput ovat tunnisteita,
///   aikaleimoja ja kertakäyttölippu.
/// - [`PendingRecord::redacted_summary`] — ihmisluettava, redaktoitu tiivistelmä
///   (esim. "`github_issue_draft` odottaa hyväksyntää"). Kutsujan **vastuulla** on
///   olla laittamatta tähän salaisuuksia; oletuksena se johdetaan vain taidon
///   nimestä ja tunnisteista.
/// - [`PendingRecord::created_at`] — luontihetki (auditointi).
///
/// Raakaa payloadia, API-avaimia, tokeneita tai KERROS B -dataa ei tallenneta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRecord {
    /// Tehtävä jota hyväksyntä koskee.
    pub task_id: ActionTaskId,
    /// Payload-sidottu hyväksyntä (kantaa vain payloadin **tiivisteen**).
    pub approval: Approval,
    /// Redaktoitu ihmisluettava tiivistelmä operaattorin näytettäväksi.
    ///
    /// Ei saa sisältää raakaa payloadia eikä salaisuuksia — vain neutraalia
    /// metatietoa (taidon nimi, mitä hyväksyntä koskee yleisellä tasolla).
    pub redacted_summary: String,
    /// Hetki jolloin odottava kirjaus luotiin (auditointi).
    pub created_at: Timestamp,
}

impl PendingRecord {
    /// Rakentaa odottavan kirjauksen tehtävälle ja sen payload-sidotulle
    /// hyväksynnälle.
    ///
    /// `redacted_summary` on kutsujan antama neutraali tiivistelmä; **sen ei saa
    /// sisältää salaisuuksia** (tallennetaan sellaisenaan levylle).
    #[must_use]
    pub fn new(
        task_id: ActionTaskId,
        approval: Approval,
        redacted_summary: impl Into<String>,
        created_at: Timestamp,
    ) -> Self {
        Self {
            task_id,
            approval,
            redacted_summary: redacted_summary.into(),
            created_at,
        }
    }

    /// Hyväksynnän tunniste (tallennuspinnan avain).
    #[must_use]
    pub fn approval_id(&self) -> ApprovalId {
        self.approval.id
    }

    /// Hetki jonka jälkeen kirjaus on vanhentunut (`approval.expires_at`).
    #[must_use]
    pub fn expires_at(&self) -> Timestamp {
        self.approval.expires_at
    }

    /// Onko kirjaus vanhentunut annettuun hetkeen `now` nähden (`now > expires_at`).
    ///
    /// Käyttää täsmälleen samaa fail-closed-vanhentumisrajaa kuin
    /// [`Approval::is_expired`]: tasan `expires_at` kelpaa vielä, aidosti
    /// myöhempi ei.
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        self.approval.is_expired(now)
    }
}

/// Kapasiteettikatto odottavien hyväksyntöjen lukumäärälle.
///
/// Estää tallennuspinnan rajattoman kasvun (muisti/levy): kun odottavia on jo
/// katon verran, uusi lisäys hylätään fail-closed. Oletus on [`PendingCapacity::DEFAULT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingCapacity(usize);

impl PendingCapacity {
    /// Oletuskatto (1024 odottavaa hyväksyntää).
    ///
    /// Käytännön human-in-the-loop-kuormalla odottavia on yleensä kourallinen;
    /// tuhannen katto antaa runsaan marginaalin mutta rajaa silti DoS-pinnan.
    pub const DEFAULT: PendingCapacity = PendingCapacity(1024);

    /// Rakentaa katon annetusta rajasta.
    ///
    /// Raja `0` tarkoittaa "ei mahdu yhtäkään" — kaikki lisäykset hylätään (voi
    /// käyttää häiriötilan kytkemiseen pois). Käytä [`PendingCapacity::DEFAULT`]
    /// jos et tarvitse erityistä rajaa.
    #[must_use]
    pub const fn new(limit: usize) -> Self {
        Self(limit)
    }

    /// Palauttaa katon numeroarvon (suurin sallittu odottavien määrä).
    #[must_use]
    pub const fn limit(self) -> usize {
        self.0
    }

    /// Mahtuuko vielä yksi lisää kun nykyinen koko on `current`.
    #[must_use]
    pub const fn has_room_for_one_more(self, current: usize) -> bool {
        current < self.0
    }
}

impl Default for PendingCapacity {
    /// Oletus on [`PendingCapacity::DEFAULT`].
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Per-olento rate-limit vaarallisille (hyväksyntää vaativille) työkalukutsuille.
///
/// Liukuva aikaikkuna: kullekin olennolle (`being`) sallitaan korkeintaan
/// `max_per_window` kirjausta `window_secs` sekunnin ikkunassa (molemmat
/// annetaan [`DangerousToolRateLimiter::new`]:lle). Tämä on tallennuspinnasta
/// riippumaton koukku jonka julkisivu voi kysyä ennen kuin se myöntää uuden
/// hyväksynnän — fail-closed-suoja sille, ettei yksi olento voi tulvittaa
/// odottavien jonoa vaarallisilla pyynnöillä.
///
/// ## Determinismi
/// Aikaleima injektoidaan ([`DangerousToolRateLimiter::check_and_record`]); kelloa
/// ei lueta sisällä. Vanhentuneet aikaleimat siivotaan laiskasti tarkistuksen
/// yhteydessä.
#[derive(Debug, Default)]
pub struct DangerousToolRateLimiter {
    /// Ikkunan pituus sekunteina.
    window_secs: i64,
    /// Suurin sallittu kirjausmäärä ikkunassa per olento.
    max_per_window: usize,
    /// Olento → viimeaikaiset kirjausaikaleimat (vanhin edessä).
    hits: Mutex<HashMap<String, VecDeque<Timestamp>>>,
}

impl DangerousToolRateLimiter {
    /// Rakentaa rajoittimen annetulla ikkunalla ja kattomäärällä.
    ///
    /// `max_per_window = 0` estää kaikki kutsut (kovakatkaisu). `window_secs <= 0`
    /// käsitellään hetkellisenä ikkunana (käytännössä jokainen kutsu on uudessa
    /// ikkunassa) — tämä ei panikoi, vaan toimii fail-open vain ikkunan osalta;
    /// käytä positiivista ikkunaa todelliseen rajoitukseen.
    #[must_use]
    pub fn new(window_secs: i64, max_per_window: usize) -> Self {
        Self {
            window_secs,
            max_per_window,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Tarkistaa onko olennolla tilaa uudelle vaaralliselle kutsulle, ja jos on,
    /// **kirjaa** sen ja palauttaa `Ok(())`. Jos kiintiö on täynnä, palauttaa
    /// [`ActionError::PolicyDenied`] **kirjaamatta** kutsua (fail-closed).
    ///
    /// Liukuva ikkuna: ennen tarkistusta vanhemmat kuin `now - window_secs`
    /// -aikaleimat häädetään. Näin laskuri seuraa vain ikkunan sisäisiä kutsuja.
    ///
    /// # Errors
    /// [`ActionError::PolicyDenied`] jos olento on jo käyttänyt kiintiönsä tässä
    /// ikkunassa.
    pub fn check_and_record(&self, being: &str, now: Timestamp) -> Result<()> {
        let cutoff = now - chrono::Duration::seconds(self.window_secs.max(0));
        let mut guard = self
            .hits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = guard.entry(being.to_string()).or_default();
        // Häädä ikkunan ulkopuoliset aikaleimat (vanhin edessä).
        while entry.front().is_some_and(|t| *t < cutoff) {
            entry.pop_front();
        }
        if entry.len() >= self.max_per_window {
            return Err(ActionError::PolicyDenied(format!(
                "vaarallisten työkalukutsujen rate-limit ylittyi olennolle '{being}' \
                 ({} / {} {}s ikkunassa)",
                entry.len(),
                self.max_per_window,
                self.window_secs
            )));
        }
        entry.push_back(now);
        Ok(())
    }

    /// Kuinka monta kirjausta olennolla on ikkunassa hetkellä `now` (häätää
    /// vanhentuneet ensin). Lähinnä testausta ja diagnostiikkaa varten.
    #[must_use]
    pub fn count_in_window(&self, being: &str, now: Timestamp) -> usize {
        let cutoff = now - chrono::Duration::seconds(self.window_secs.max(0));
        let mut guard = self
            .hits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = guard.get_mut(being) else {
            return 0;
        };
        while entry.front().is_some_and(|t| *t < cutoff) {
            entry.pop_front();
        }
        entry.len()
    }
}

/// Odottavien hyväksyntöjen tallennuspinta.
///
/// Abstrahoi sen, **missä** odottavat hyväksynnät elävät — prosessin muistissa
/// vai kaatumiskestävällä levyllä — jotta [`crate::facade::ActionRuntime`] voi
/// vaihtaa tallennustaustan rikkomatta logiikkaansa. Kaikki metodit ovat `&self`
/// (sisäinen mutaatio lukon takana), jotta trait on `dyn`-yhteensopiva.
///
/// ## Sopimus
/// - [`insert`](PendingApprovalStore::insert) **kunnioittaa kapasiteettikattoa**:
///   jos pinta on jo täynnä, lisäys hylätään fail-closed
///   ([`ActionError::PolicyDenied`]) eikä mitään kirjoiteta.
/// - [`get`](PendingApprovalStore::get) / [`remove`](PendingApprovalStore::remove)
///   palauttavat tallennetun [`PendingRecord`]:n koko muodossaan (sisältää
///   payload-sidotun hyväksynnän), jotta `approve` voi jatkaa suoritusta.
/// - [`remove`](PendingApprovalStore::remove) on **kertakäyttöinen**: kulutettua
///   tunnistetta ei enää löydy (sama nonce-semantiikka kuin
///   [`crate::approval`]).
/// - [`list`](PendingApprovalStore::list) palauttaa kaikki odottavat kirjaukset
///   (operaattorin pinta + kapasiteetin laskenta).
/// - [`evict_expired`](PendingApprovalStore::evict_expired) poistaa vanhentuneet
///   kirjaukset fail-closed-rajalla.
///
/// ## Salaisuudet
/// Toteutus joka tallentaa levylle saa kirjoittaa **vain** [`PendingRecord`]:n
/// salaisuudettomat kentät (tiiviste + tunnisteet + redaktoitu tiivistelmä) —
/// ei koskaan raakaa payloadia.
pub trait PendingApprovalStore: Send + Sync {
    /// Lisää odottavan kirjauksen, **jos** kapasiteettikatto ei ylity.
    ///
    /// Avain on `record.approval_id()`. Saman tunnisteen uudelleenlisäys korvaa
    /// aiemman (käytännössä tunnisteet ovat uniikkeja). Lisäys lasketaan
    /// kapasiteettia vasten vain kun kyseessä on **uusi** tunniste.
    ///
    /// # Errors
    /// [`ActionError::PolicyDenied`] jos kapasiteettikatto ([`PendingCapacity`])
    /// estää uuden tunnisteen lisäämisen. Levytoteutuksilla lisäksi I/O-virhe
    /// ([`ActionError::Proof`]) jos journaliin kirjoitus epäonnistuu.
    fn insert(&self, record: PendingRecord) -> Result<()>;

    /// Hakee odottavan kirjauksen hyväksynnän tunnisteella; `None` jos ei löydy
    /// (tai se on jo kulutettu/häädetty).
    ///
    /// Palauttaa kopion koko kirjauksesta (ei viitettä), jotta toteutus voi pitää
    /// sisäisen lukon vain haun ajan.
    ///
    /// # Errors
    /// Levytoteutuksilla [`ActionError::Proof`] jos lokin luku epäonnistuu.
    fn get(&self, approval_id: ApprovalId) -> Result<Option<PendingRecord>>;

    /// Poistaa (kuluttaa) odottavan kirjauksen ja palauttaa sen, jos se oli
    /// olemassa; `None` jos sitä ei ollut.
    ///
    /// Kertakäyttöinen: poiston jälkeen sama tunniste ei enää löydy
    /// [`get`](PendingApprovalStore::get):llä. Levytoteutuksilla poisto on pysyvä
    /// (kaatumisen yli).
    ///
    /// # Errors
    /// Levytoteutuksilla [`ActionError::Proof`] jos poistomerkinnän kirjoitus
    /// epäonnistuu.
    fn remove(&self, approval_id: ApprovalId) -> Result<Option<PendingRecord>>;

    /// Odottavien kirjausten lukumäärä.
    ///
    /// # Errors
    /// Levytoteutuksilla [`ActionError::Proof`] jos lokin luku epäonnistuu.
    fn len(&self) -> Result<usize>;

    /// Onko pinta tyhjä (ei yhtäkään odottavaa kirjausta).
    ///
    /// # Errors
    /// Sama kuin [`len`](PendingApprovalStore::len).
    fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Luettelee kaikki odottavat kirjaukset.
    ///
    /// Käytetään sekä operaattorin pinnan ([`crate::facade::ActionRuntime::pending_approvals`])
    /// että kapasiteettikaton laskennassa. Järjestystä ei taata; kutsuja
    /// vakauttaa sen tarvittaessa (esim. hyväksynnän tunnisteen mukaan).
    ///
    /// # Errors
    /// Levytoteutuksilla [`ActionError::Proof`] jos lokin luku epäonnistuu.
    fn list(&self) -> Result<Vec<PendingRecord>>;

    /// Poistaa kaikki annettuun hetkeen `now` mennessä vanhentuneet kirjaukset
    /// ja palauttaa häädettyjen lukumäärän.
    ///
    /// Käyttää täsmälleen samaa fail-closed-vanhentumisrajaa kuin
    /// [`crate::approval`] ([`PendingRecord::is_expired`]): `now > expires_at`.
    /// Vanhentunutta hyväksyntää ei voi enää kuluttaa, joten sen säilyttäminen
    /// olisi vain roskaa tallennuspinnalla.
    ///
    /// # Errors
    /// Levytoteutuksilla [`ActionError::Proof`] jos lokin luku/kirjoitus
    /// epäonnistuu.
    fn evict_expired(&self, now: Timestamp) -> Result<usize>;
}

/// Muistinvarainen tallennuspinta ([`HashMap`] traitin takana).
///
/// Oletus ja testikäyttö: nopea ja yksinkertainen, **mutta ei selviä prosessin
/// kaatumisesta** — kaatuessa kaikki odottavat hyväksynnät katoavat. Tuotannossa
/// jossa kaatumiskestävyys on vaatimus, käytä [`JournalPendingStore`]:a.
#[derive(Debug)]
pub struct InMemoryPendingStore {
    /// Hyväksynnän tunniste → odottava kirjaus.
    inner: Mutex<HashMap<ApprovalId, PendingRecord>>,
    /// Kapasiteettikatto.
    capacity: PendingCapacity,
}

impl InMemoryPendingStore {
    /// Luo tyhjän muistipinnan oletuskapasiteetilla ([`PendingCapacity::DEFAULT`]).
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(PendingCapacity::DEFAULT)
    }

    /// Luo tyhjän muistipinnan annetulla kapasiteettikatolla.
    #[must_use]
    pub fn with_capacity(capacity: PendingCapacity) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            capacity,
        }
    }

    /// Lukitsee sisäisen kartan, toipuen myrkytetystä lukosta paniikkaamatta.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ApprovalId, PendingRecord>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Default for InMemoryPendingStore {
    /// Oletus on tyhjä pinta oletuskapasiteetilla.
    fn default() -> Self {
        Self::new()
    }
}

impl PendingApprovalStore for InMemoryPendingStore {
    fn insert(&self, record: PendingRecord) -> Result<()> {
        let mut map = self.lock();
        let id = record.approval_id();
        // Kapasiteetti lasketaan vain UUSILLE tunnisteille: olemassa olevan
        // korvaaminen ei kasvata kokoa.
        if !map.contains_key(&id) && !self.capacity.has_room_for_one_more(map.len()) {
            return Err(ActionError::PolicyDenied(format!(
                "odottavien hyväksyntöjen kapasiteettikatto {} täynnä",
                self.capacity.limit()
            )));
        }
        map.insert(id, record);
        Ok(())
    }

    fn get(&self, approval_id: ApprovalId) -> Result<Option<PendingRecord>> {
        Ok(self.lock().get(&approval_id).cloned())
    }

    fn remove(&self, approval_id: ApprovalId) -> Result<Option<PendingRecord>> {
        Ok(self.lock().remove(&approval_id))
    }

    fn len(&self) -> Result<usize> {
        Ok(self.lock().len())
    }

    fn list(&self) -> Result<Vec<PendingRecord>> {
        Ok(self.lock().values().cloned().collect())
    }

    fn evict_expired(&self, now: Timestamp) -> Result<usize> {
        let mut map = self.lock();
        let before = map.len();
        map.retain(|_, record| !record.is_expired(now));
        Ok(before - map.len())
    }
}

/// Journal-rivin looginen nimi odottavan kirjauksen lisäykselle.
const PENDING_PUT: &str = "pending_approval_put";
/// Journal-rivin looginen nimi odottavan kirjauksen poistolle (tombstone).
const PENDING_DELETE: &str = "pending_approval_delete";

/// Kaatumiskestävä tallennuspinta [`familyclaw_durable::FileJournal`]:n päällä.
///
/// Append-only-loki: jokainen lisäys kirjoitetaan `pending_approval_put`-markerina
/// (sisältää koko salaisuudettoman [`PendingRecord`]:n) ja jokainen poisto
/// `pending_approval_delete`-markerina (sisältää vain hyväksynnän tunnisteen,
/// tombstone). Tila rekonstruoidaan toistamalla loki: myöhempi rivi voittaa,
/// joten poisto kumoaa aiemman lisäyksen.
///
/// Koska [`FileJournal::append`] flushaa ja fsyncaa ennen paluuta, valmistunut
/// lisäys/poisto on levyllä myös äkillisen kaatumisen jälkeen. Avattaessa
/// `FileJournal` eheyttää kaatumisen jättämän vajaan viimeisen rivin, joten
/// loki säilyy luettavana. Näin **odottava hyväksyntä selviää
/// `submit-task`:n ja `approve`:n välisestä kaatumisesta**.
///
/// ## Salaisuusinvariantti
/// Levylle kirjoitetaan vain [`PendingRecord`]:n salaisuudettomat kentät
/// (payloadin tiiviste, tunnisteet, redaktoitu tiivistelmä, aikaleimat) — ei
/// koskaan raakaa payloadia.
pub struct JournalPendingStore {
    /// Append-only-loki johon lisäykset ja poistot kirjataan.
    journal: FileJournal,
    /// Seuraavan rivin sekvenssipaikka (monotoninen).
    next_step: Mutex<StepId>,
    /// Kapasiteettikatto.
    capacity: PendingCapacity,
}

impl std::fmt::Debug for JournalPendingStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalPendingStore")
            .field("path", &self.journal.path())
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

impl JournalPendingStore {
    /// Avaa (tai luo) kaatumiskestävän pinnan annetusta tiedostopolusta
    /// oletuskapasiteetilla.
    ///
    /// Olemassa olevasta lokista odottavat hyväksynnät rekonstruoidaan heti, joten
    /// uudelleenkäynnistyksen jälkeen ne ovat yhä [`get`](PendingApprovalStore::get)-
    /// haettavissa ja hyväksyttävissä.
    ///
    /// # Errors
    /// [`ActionError::Proof`] jos journalia ei voi avata tai sen lukeminen
    /// (sekvenssipaikan päättelyä varten) epäonnistuu.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_capacity(path, PendingCapacity::DEFAULT)
    }

    /// Avaa (tai luo) pinnan annetulla kapasiteettikatolla.
    ///
    /// # Errors
    /// [`ActionError::Proof`] jos journalia ei voi avata tai lukea.
    pub fn open_with_capacity(
        path: impl AsRef<Path>,
        capacity: PendingCapacity,
    ) -> Result<Self> {
        let journal = FileJournal::open(path)
            .map_err(|e| ActionError::Proof(format!("open pending journal failed: {e}")))?;
        // Päättele seuraava sekvenssipaikka olemassa olevan lokin pituudesta.
        let len = journal
            .len()
            .map_err(|e| ActionError::Proof(format!("read pending journal failed: {e}")))?;
        let next = StepId::new(u64::try_from(len).unwrap_or(u64::MAX));
        Ok(Self {
            journal,
            next_step: Mutex::new(next),
            capacity,
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

    /// Liittää markerin lokiin annetulla nimellä ja hyötykuormalla.
    fn append_marker(&self, name: &str, payload: serde_json::Value) -> Result<()> {
        let entry = JournalEntry::marker(self.next_step_id(), name, payload);
        self.journal
            .append(entry)
            .map_err(|e| ActionError::Proof(format!("append pending marker failed: {e}")))
    }

    /// Rekonstruoi nykytilan (hyväksynnän tunniste → kirjaus) toistamalla lokin.
    ///
    /// Toisto käy rivit järjestyksessä: `pending_approval_put` lisää/korvaa
    /// kirjauksen, `pending_approval_delete` poistaa sen (tombstone). Muut rivit
    /// ohitetaan. Näin myöhempi rivi voittaa ja poisto kumoaa lisäyksen.
    fn replay_state(&self) -> Result<HashMap<ApprovalId, PendingRecord>> {
        let entries = self
            .journal
            .replay_all()
            .map_err(|e| ActionError::Proof(format!("replay pending journal failed: {e}")))?;
        let mut state: HashMap<ApprovalId, PendingRecord> = HashMap::new();
        for entry in entries {
            let EntryKind::Marker { name, payload } = entry.kind else {
                continue;
            };
            match name.as_str() {
                PENDING_PUT => {
                    let record: PendingRecord = serde_json::from_value(payload).map_err(|e| {
                        ActionError::Proof(format!("decode pending put record failed: {e}"))
                    })?;
                    state.insert(record.approval_id(), record);
                }
                PENDING_DELETE => {
                    let id: ApprovalId = serde_json::from_value(payload).map_err(|e| {
                        ActionError::Proof(format!("decode pending delete id failed: {e}"))
                    })?;
                    state.remove(&id);
                }
                _ => {}
            }
        }
        Ok(state)
    }
}

impl PendingApprovalStore for JournalPendingStore {
    fn insert(&self, record: PendingRecord) -> Result<()> {
        // Kapasiteetti tarkistetaan rekonstruoitua tilaa vasten; uusi tunniste
        // ei mahdu jos pinta on jo täynnä (olemassa olevan korvaus sallitaan).
        let state = self.replay_state()?;
        let id = record.approval_id();
        if !state.contains_key(&id) && !self.capacity.has_room_for_one_more(state.len()) {
            return Err(ActionError::PolicyDenied(format!(
                "odottavien hyväksyntöjen kapasiteettikatto {} täynnä",
                self.capacity.limit()
            )));
        }
        let payload = serde_json::to_value(&record)
            .map_err(|e| ActionError::Proof(format!("encode pending record failed: {e}")))?;
        self.append_marker(PENDING_PUT, payload)
    }

    fn get(&self, approval_id: ApprovalId) -> Result<Option<PendingRecord>> {
        Ok(self.replay_state()?.remove(&approval_id))
    }

    fn remove(&self, approval_id: ApprovalId) -> Result<Option<PendingRecord>> {
        let existing = self.replay_state()?.remove(&approval_id);
        if existing.is_some() {
            // Kirjaa tombstone vain jos kirjaus oli olemassa — turha rivi vältetään.
            let payload = serde_json::to_value(approval_id).map_err(|e| {
                ActionError::Proof(format!("encode pending delete id failed: {e}"))
            })?;
            self.append_marker(PENDING_DELETE, payload)?;
        }
        Ok(existing)
    }

    fn len(&self) -> Result<usize> {
        Ok(self.replay_state()?.len())
    }

    fn list(&self) -> Result<Vec<PendingRecord>> {
        Ok(self.replay_state()?.into_values().collect())
    }

    fn evict_expired(&self, now: Timestamp) -> Result<usize> {
        let state = self.replay_state()?;
        let expired: Vec<ApprovalId> = state
            .values()
            .filter(|record| record.is_expired(now))
            .map(PendingRecord::approval_id)
            .collect();
        for id in &expired {
            let payload = serde_json::to_value(id).map_err(|e| {
                ActionError::Proof(format!("encode pending delete id failed: {e}"))
            })?;
            self.append_marker(PENDING_DELETE, payload)?;
        }
        Ok(expired.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::sha256_hex;
    use crate::ids::ActionId;
    use chrono::Duration;
    use familyclaw_core::time::from_unix_secs;
    use std::path::PathBuf;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    /// Apuri: payload-sidottu hyväksyntä annetulla TTL:llä.
    fn approval_at(now: Timestamp, ttl: Duration) -> Approval {
        let payload = serde_json::to_vec(&serde_json::json!({ "to": "general" }))
            .expect("serialize payload");
        Approval {
            id: ApprovalId::new(),
            action_id: ActionId::new(),
            payload_hash: sha256_hex(&payload),
            granted_at: now,
            expires_at: now + ttl,
            consumed: false,
        }
    }

    /// Apuri: odottava kirjaus annetulla TTL:llä.
    fn record_at(now: Timestamp, ttl: Duration) -> PendingRecord {
        PendingRecord::new(
            ActionTaskId::new(),
            approval_at(now, ttl),
            "github_issue_draft odottaa hyväksyntää",
            now,
        )
    }

    /// RAII-temp-tiedosto ilman ulkoisia crateja.
    struct TempPath(PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let unique = format!(
                "familyclaw-pending-{tag}-{}-{:?}.jsonl",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            );
            p.push(unique);
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

    // ---- In-memory: insert + get + remove ----

    #[test]
    fn in_memory_insert_get_remove_roundtrip() {
        let store = InMemoryPendingStore::new();
        let now = at(1_700_000_000);
        let record = record_at(now, Duration::minutes(60));
        let id = record.approval_id();
        let task_id = record.task_id;

        store.insert(record).expect("insert");
        assert_eq!(store.len().expect("len"), 1);

        let got = store.get(id).expect("get ok").expect("present");
        assert_eq!(got.approval_id(), id);
        assert_eq!(got.task_id, task_id);

        let removed = store.remove(id).expect("remove ok").expect("was present");
        assert_eq!(removed.approval_id(), id);
        // Kertakäyttö: poiston jälkeen ei enää löydy.
        assert!(store.get(id).expect("get ok").is_none());
        assert!(store.remove(id).expect("remove ok").is_none());
        assert!(store.is_empty().expect("empty"));
    }

    #[test]
    fn in_memory_get_missing_is_none() {
        let store = InMemoryPendingStore::new();
        assert!(store.get(ApprovalId::new()).expect("get").is_none());
    }

    #[test]
    fn in_memory_list_returns_all() {
        let store = InMemoryPendingStore::new();
        let now = at(1_700_000_000);
        for _ in 0..3 {
            store
                .insert(record_at(now, Duration::minutes(60)))
                .expect("insert");
        }
        assert_eq!(store.list().expect("list").len(), 3);
    }

    // ---- Capacity cap ----

    #[test]
    fn capacity_cap_rejects_beyond_limit() {
        let store = InMemoryPendingStore::with_capacity(PendingCapacity::new(2));
        let now = at(1_700_000_000);

        store
            .insert(record_at(now, Duration::minutes(60)))
            .expect("first fits");
        store
            .insert(record_at(now, Duration::minutes(60)))
            .expect("second fits");

        // Kolmas ylittää katon → fail-closed.
        let err = store
            .insert(record_at(now, Duration::minutes(60)))
            .expect_err("third exceeds cap");
        assert!(matches!(err, ActionError::PolicyDenied(_)));
        assert_eq!(store.len().expect("len"), 2);
    }

    #[test]
    fn capacity_cap_allows_replacing_existing_id() {
        let store = InMemoryPendingStore::with_capacity(PendingCapacity::new(1));
        let now = at(1_700_000_000);
        let record = record_at(now, Duration::minutes(60));
        let id = record.approval_id();

        store.insert(record.clone()).expect("first");
        // Saman tunnisteen uudelleenlisäys ei kasvata kokoa → ei rikota kattoa.
        store.insert(record).expect("replace same id under cap");
        assert_eq!(store.len().expect("len"), 1);
        // Sama tunniste on yhä haettavissa korvauksen jälkeen.
        assert!(store.get(id).expect("get").is_some());
    }

    // ---- TTL eviction ----

    #[test]
    fn ttl_eviction_drops_expired_only() {
        let store = InMemoryPendingStore::new();
        let now = at(1_700_000_000);

        // Yksi vanhentuu 60s päästä, toinen 3600s päästä.
        let short = record_at(now, Duration::seconds(60));
        let long = record_at(now, Duration::seconds(3600));
        let short_id = short.approval_id();
        let long_id = long.approval_id();
        store.insert(short).expect("insert short");
        store.insert(long).expect("insert long");

        // now + 120s: lyhyt vanhentunut, pitkä ei.
        let evicted = store.evict_expired(at(1_700_000_120)).expect("evict");
        assert_eq!(evicted, 1);
        assert!(store.get(short_id).expect("get").is_none());
        assert!(store.get(long_id).expect("get").is_some());
    }

    #[test]
    fn ttl_eviction_boundary_keeps_exactly_at_expiry() {
        let store = InMemoryPendingStore::new();
        let now = at(1_700_000_000);
        let record = record_at(now, Duration::seconds(60));
        let id = record.approval_id();
        store.insert(record).expect("insert");

        // Tasan expires_at (now+60) EI vanhentunut (sama fail-closed-raja kuin approval.rs).
        assert_eq!(store.evict_expired(at(1_700_000_060)).expect("evict"), 0);
        assert!(store.get(id).expect("get").is_some());
        // Yksi sekunti rajan jälkeen → häädetään.
        assert_eq!(store.evict_expired(at(1_700_000_061)).expect("evict"), 1);
        assert!(store.get(id).expect("get").is_none());
    }

    // ---- Durable: reload across simulated restart ----

    #[test]
    fn durable_reloads_pending_after_simulated_restart() {
        let tmp = TempPath::new("reload");
        let now = at(1_700_000_000);
        let record = record_at(now, Duration::minutes(60));
        let id = record.approval_id();
        let task_id = record.task_id;
        let payload_hash = record.approval.payload_hash.clone();

        // Vaihe 1: kirjoita kirjaus pintaan ja PUDOTA se (simuloi kaatuminen).
        {
            let store = JournalPendingStore::open(tmp.path()).expect("open 1");
            store.insert(record).expect("insert");
            assert_eq!(store.len().expect("len"), 1);
        } // store droppataan = prosessi "kaatuu"

        // Vaihe 2: luo pinta UUDELLEEN samasta tiedostosta — kirjaus säilyi.
        let resumed = JournalPendingStore::open(tmp.path()).expect("open 2");
        assert_eq!(resumed.len().expect("len"), 1, "pending survived restart");
        let got = resumed.get(id).expect("get").expect("still present");
        assert_eq!(got.approval_id(), id);
        assert_eq!(got.task_id, task_id);
        // Payload-sidonta säilyi: tiiviste on yhä sama (approve voi kuluttaa).
        assert_eq!(got.approval.payload_hash, payload_hash);
        assert!(!got.approval.consumed, "not yet consumed → approvable");

        // Approvable: poisto kuluttaa sen pysyvästi.
        let removed = resumed.remove(id).expect("remove").expect("present");
        assert_eq!(removed.approval_id(), id);

        // Vaihe 3: vielä yksi restart — poisto myös säilyi (tombstone).
        let after_remove = JournalPendingStore::open(tmp.path()).expect("open 3");
        assert!(after_remove.get(id).expect("get").is_none());
        assert!(after_remove.is_empty().expect("empty"));
    }

    #[test]
    fn durable_persisted_form_contains_no_raw_secret() {
        let tmp = TempPath::new("no-secret");
        let now = at(1_700_000_000);

        // Rakenna kirjaus jonka payload SISÄLSI salaisuuden — mutta vain tiiviste
        // tallennetaan, ei raakaa arvoa.
        let secret = format!("sk-{}", "live".repeat(4));
        let payload =
            serde_json::to_vec(&serde_json::json!({ "api_key": secret })).expect("serialize");
        let approval = Approval {
            id: ApprovalId::new(),
            action_id: ActionId::new(),
            payload_hash: sha256_hex(&payload),
            granted_at: now,
            expires_at: now + Duration::minutes(60),
            consumed: false,
        };
        let record = PendingRecord::new(
            ActionTaskId::new(),
            approval,
            "skill odottaa hyväksyntää",
            now,
        );

        let store = JournalPendingStore::open(tmp.path()).expect("open");
        store.insert(record).expect("insert");

        // Levyltä luettu raakateksti EI saa sisältää salaisuutta.
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read journal file");
        assert!(
            !on_disk.contains(&secret),
            "persisted journal must never contain the raw secret"
        );
        assert!(!on_disk.contains("sk-livelivelivelive"));
        // Mutta tiiviste ON läsnä (payload-sidonta säilyy).
        assert!(on_disk.contains(&sha256_hex(&payload)));
    }

    #[test]
    fn durable_capacity_cap_rejects_beyond_limit() {
        let tmp = TempPath::new("cap");
        let now = at(1_700_000_000);
        let store =
            JournalPendingStore::open_with_capacity(tmp.path(), PendingCapacity::new(1))
                .expect("open");

        store
            .insert(record_at(now, Duration::minutes(60)))
            .expect("first fits");
        let err = store
            .insert(record_at(now, Duration::minutes(60)))
            .expect_err("second exceeds cap");
        assert!(matches!(err, ActionError::PolicyDenied(_)));
        assert_eq!(store.len().expect("len"), 1);
    }

    #[test]
    fn durable_evict_expired_persists_across_restart() {
        let tmp = TempPath::new("evict");
        let now = at(1_700_000_000);
        let short = record_at(now, Duration::seconds(60));
        let short_id = short.approval_id();
        let long = record_at(now, Duration::seconds(3600));
        let long_id = long.approval_id();

        {
            let store = JournalPendingStore::open(tmp.path()).expect("open 1");
            store.insert(short).expect("insert short");
            store.insert(long).expect("insert long");
            let evicted = store.evict_expired(at(1_700_000_120)).expect("evict");
            assert_eq!(evicted, 1);
        }
        // Restart: häätö säilyi.
        let resumed = JournalPendingStore::open(tmp.path()).expect("open 2");
        assert!(resumed.get(short_id).expect("get").is_none());
        assert!(resumed.get(long_id).expect("get").is_some());
        assert_eq!(resumed.len().expect("len"), 1);
    }

    // ---- Rate limiter ----

    #[test]
    fn rate_limiter_allows_up_to_cap_then_denies() {
        let limiter = DangerousToolRateLimiter::new(60, 2);
        let now = at(1_700_000_000);

        limiter.check_and_record("being-a", now).expect("first");
        limiter.check_and_record("being-a", now).expect("second");
        // Kolmas ikkunassa → estetään.
        let err = limiter
            .check_and_record("being-a", now)
            .expect_err("third denied");
        assert!(matches!(err, ActionError::PolicyDenied(_)));
        assert_eq!(limiter.count_in_window("being-a", now), 2);
    }

    #[test]
    fn rate_limiter_is_per_being() {
        let limiter = DangerousToolRateLimiter::new(60, 1);
        let now = at(1_700_000_000);
        limiter.check_and_record("being-a", now).expect("being-a first");
        // Eri olento → oma kiintiö.
        limiter.check_and_record("being-b", now).expect("being-b first");
        // being-a jo täynnä.
        assert!(limiter.check_and_record("being-a", now).is_err());
    }

    #[test]
    fn rate_limiter_window_slides() {
        let limiter = DangerousToolRateLimiter::new(60, 1);
        let now = at(1_700_000_000);
        limiter.check_and_record("being-a", now).expect("first");
        assert!(limiter.check_and_record("being-a", now).is_err());
        // Ikkunan jälkeen (now + 61s) vanha kirjaus häätyy → tilaa taas.
        limiter
            .check_and_record("being-a", at(1_700_000_061))
            .expect("after window slides");
    }
}
