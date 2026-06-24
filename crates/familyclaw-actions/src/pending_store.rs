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
//! - **Per-olento-rate-limit** ([`DangerousToolRateLimiter`]): laskuri
//!   vaarallisten (hyväksyntää vaativien) työkalukutsujen rajoittamiseen
//!   liukuvalla aikaikkunalla. **Kytketty hyväksyntäpolkuun**
//!   ([`crate::facade::ActionRuntime::submit_task`]): kun tehtävä jäisi
//!   odottamaan ihmisen hyväksyntää, julkisivu kysyy ensin tältä rajoittimelta
//!   onko olennolla vielä tilaa — jos ei, hyväksyntää ei myönnetä vaan kutsu
//!   hylätään fail-closed ([`ActionError::PolicyDenied`]). Globaali
//!   kapasiteettikatto rajaa koko jonon; tämä lisää siihen **per-olento**-katon.
//!   Auto-run-tehtäviä (luku / paikallinen kirjoitus) ei rate-limititä.
//!   Deterministinen: aikaleima injektoidaan.
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
/// riippumaton koukku jonka julkisivu ([`crate::facade::ActionRuntime`]) kysyy
/// `submit-task`:ssa **ennen** kuin se myöntää uuden hyväksynnän — fail-closed-
/// suoja sille, ettei yksi olento voi tulvittaa odottavien jonoa vaarallisilla
/// pyynnöillä. Globaali kapasiteettikatto ([`PendingCapacity`]) rajaa koko jonon;
/// tämä lisää siihen **per-olento**-katon.
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

    /// Palauttaa pinnan **lajitunnisteen** (`"in-memory"` tai `"journal"`).
    ///
    /// Tämä on salaisuudeton tarkistuskoukku kokoojalle ja testeille: sillä voi
    /// todeta että persistentti kokoonpano sai kaatumiskestävän (`"journal"`)
    /// odottavien hyväksyntöjen pinnan oletuksellisen muistinvaraisen
    /// (`"in-memory"`) sijaan, paljastamatta sisäistä tilaa tai tiedostopolkua.
    /// Sama tarkoitus kuin [`crate::dispatch_outbox::DispatchOutboxStore::kind`]:lla.
    /// Oletus on `"in-memory"`; kaatumiskestävät toteutukset ohittavat tämän.
    fn kind(&self) -> &'static str {
        "in-memory"
    }
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

/// Tiivistyksen oletuskerroin: loki tiivistetään automaattisesti kun fyysisten
/// rivien määrä ylittää `AUTO_COMPACT_FACTOR * elävien_kirjausten_määrä`.
///
/// Kerroin 2 tarkoittaa "tiivistä kun vähintään puolet riveistä on kuolleita"
/// (poistettuja tai korvattuja). Tämä rajaa kuolleiden rivien kertymisen
/// vakiokertoimeen elävää kohti, joten lokin koko ja replayn O(n)-kustannus
/// pysyvät elävän tilan kokoluokassa rajattoman kasvun sijaan.
const AUTO_COMPACT_FACTOR: usize = 2;

/// Pienin fyysinen rivimäärä jolla auto-tiivistys ylipäänsä harkitaan.
///
/// Estää turhan tiivistyksen pienillä lokeilla (esim. 1 elävä + 1 tombstone =
/// 2 riviä laukaisisi muuten heti). Vasta kun rivejä on tämän verran,
/// dead-row-suhdetta aletaan valvoa.
const AUTO_COMPACT_MIN_ROWS: usize = 64;

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
/// ## Tiivistys (compaction) — rajaton kasvu kuriin
/// Koska loki on append-only, jokainen poisto ([`remove`](PendingApprovalStore::remove)
/// / [`evict_expired`](PendingApprovalStore::evict_expired)) ja saman tunnisteen
/// korvaus jättää **kuolleita rivejä** lokiin: tila on yhä oikea (myöhempi rivi
/// voittaa replayssa), mutta tiedosto kasvaa rajatta ja replay muuttuu O(n):ksi
/// rivimäärässä — ei elävien kirjausten määrässä.
/// [`compact`](JournalPendingStore::compact) kirjoittaa lokin uudelleen
/// sisältämään **vain elävät kirjaukset** (kuolleet/tombstonatut/korvatut rivit
/// pudotetaan) atomisesti [`FileJournal::rewrite`]:n kautta — elävä tila säilyy
/// bitilleen, eikä keskeytyminen koskaan menetä eläviä rivejä (rename-pohjainen
/// swap). Tiivistys laukeaa joko operaattorin kutsumana
/// ([`compact`](JournalPendingStore::compact)) tai **automaattisesti** lisäyksen
/// ja häädön yhteydessä kun kuolleiden rivien osuus ylittää kynnyksen (ks.
/// `AUTO_COMPACT_FACTOR` ja [`with_auto_compact_factor`](JournalPendingStore::with_auto_compact_factor)).
///
/// ## Salaisuusinvariantti
/// Levylle kirjoitetaan vain [`PendingRecord`]:n salaisuudettomat kentät
/// (payloadin tiiviste, tunnisteet, redaktoitu tiivistelmä, aikaleimat) — ei
/// koskaan raakaa payloadia. Tiivistys säilyttää tämän: uudelleenkirjoitettu loki
/// sisältää samat salaisuudettomat `pending_approval_put`-rivit.
pub struct JournalPendingStore {
    /// Append-only-loki johon lisäykset ja poistot kirjataan.
    journal: FileJournal,
    /// Seuraavan rivin sekvenssipaikka (monotoninen).
    next_step: Mutex<StepId>,
    /// Kapasiteettikatto.
    capacity: PendingCapacity,
    /// Auto-tiivistyksen kerroin: tiivistä kun `rivit > factor * elävät`.
    /// `0` poistaa auto-tiivistyksen käytöstä (vain manuaalinen `compact`).
    auto_compact_factor: usize,
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
    pub fn open_with_capacity(path: impl AsRef<Path>, capacity: PendingCapacity) -> Result<Self> {
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
            auto_compact_factor: AUTO_COMPACT_FACTOR,
        })
    }

    /// Asettaa auto-tiivistyksen kertoimen (ketjutus).
    ///
    /// Loki tiivistetään automaattisesti kun fyysisten rivien määrä ylittää
    /// `factor * elävien_kirjausten_määrä` (ja rivejä on vähintään
    /// `AUTO_COMPACT_MIN_ROWS`). Oletus on `AUTO_COMPACT_FACTOR` (2). Arvo `0`
    /// **poistaa** auto-tiivistyksen käytöstä — tällöin loki tiivistetään vain
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
        Self::reconstruct_state(entries)
    }

    /// Rakentaa nykytilan annetuista journal-riveistä (puhdas funktio, ei I/O).
    ///
    /// Toisto käy rivit järjestyksessä: `pending_approval_put` lisää/korvaa
    /// kirjauksen, `pending_approval_delete` poistaa sen (tombstone). Muut rivit
    /// ohitetaan. Myöhempi rivi voittaa, joten poisto kumoaa aiemman lisäyksen.
    /// Eriytetty [`replay_state`](Self::replay_state):stä jotta sekä levyltä
    /// lukeva replay että [`compact`](Self::compact):n
    /// [`FileJournal::compact_with`]-suljin voivat rakentaa tilan **samalla
    /// logiikalla** — jälkimmäinen saa rivit valmiiksi luettuina lukon alta, eikä
    /// saa lukea journalia uudelleen (deadlock).
    fn reconstruct_state(entries: Vec<JournalEntry>) -> Result<HashMap<ApprovalId, PendingRecord>> {
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

    /// Fyysisten journal-rivien määrä (eläviä + kuolleita). Tämä on se luku jota
    /// vasten dead-row-suhde mitataan; eroaa [`len`](PendingApprovalStore::len):
    /// stä joka palauttaa vain elävien kirjausten määrän.
    fn physical_row_count(&self) -> Result<usize> {
        self.journal
            .len()
            .map_err(|e| ActionError::Proof(format!("read pending journal len failed: {e}")))
    }

    /// Kirjoittaa lokin uudelleen sisältämään **vain elävät kirjaukset**
    /// (tiivistys), pudottaen kaikki kuolleet rivit (tombstonet ja korvatut
    /// `put`-rivit). Palauttaa pudotettujen kuolleiden rivien määrän.
    ///
    /// Elävä tila säilyy bitilleen: tiivistyksen jälkeen täsmälleen samat
    /// hyväksynnät ovat [`get`](PendingApprovalStore::get)-haettavissa, ja
    /// uudelleenlatauksesta (restart) rekonstruoituu identtinen tila. Tiivistys
    /// on **atominen** ([`FileJournal::rewrite`]: temp + fsync + rename) — jos
    /// prosessi kaatuu kesken, elävä tiedosto on yhä ehjässä vanhassa tilassaan
    /// eikä yhtään elävää hyväksyntää katoa.
    ///
    /// Rivit uudelleennumeroidaan tiiviiksi `0..N`-sekvenssiksi, ja sisäinen
    /// sekvenssikursori asetetaan vastaamaan, jotta tulevat lisäykset jatkavat
    /// oikealta paikalta.
    ///
    /// # Errors
    /// [`ActionError::Proof`] jos lokin luku, rivien sarjallistus tai atominen
    /// uudelleenkirjoitus epäonnistuu. Virhetilanteessa elävä loki jätetään
    /// entiselleen (rewrite ei koske elävään tiedostoon ennen kuin temp on ehjä).
    pub fn compact(&self) -> Result<usize> {
        // Atominen tiivistys appendeja vastaan: [`FileJournal::compact_with`]
        // pitää saman file-lukon koko luku→suodatus→swap-operaation ajan, joten
        // rinnakkainen lisäys/poisto ei voi laskeutua aukkoon ja kadota
        // (TOCTOU-korjaus). `build`-suljin saa luetut rivit, rekonstruoi elävän
        // tilan ja palauttaa uudelleennumeroidut elävät PENDING_PUT-rivit.
        //
        // Sekvenssikursorin asetus tehdään ERIKSEEN sulkimen ulkopuolella: suljin
        // EI saa lukita journalia uudelleen, mutta `next_step` on eri mutex kuin
        // file-lukko, joten sen päivittäminen sulkimen sisältä OLISI turvallista —
        // mutta tehdään se silti `compact_with`:n PALUUN jälkeen jotta kursori
        // päivittyy vain kun swap tosiasiassa onnistui.
        // Elävien rivien määrä smugletaan sulkimesta `Cell`:llä, jotta
        // sekvenssikursori voidaan asettaa swapin jälkeen (suljin EI saa lukita
        // journalia uudelleen → ei voi lukea kursoria omalta polultaan).
        let live_count = std::cell::Cell::new(0usize);
        let dropped = self
            .journal
            .compact_with(|entries| {
                // Rekonstruoi elävä tila valmiiksi luetuista riveistä (sama
                // logiikka kuin replayssa, mutta EI uudelleenlukua — uudelleenluku
                // lukitsisi journalin ja deadlockkaisi). ActionError kääritään
                // DurableError-tekstiksi jotta tyyppi sopii compact_with-sopimukseen.
                let state = Self::reconstruct_state(entries).map_err(|e| {
                    familyclaw_durable::DurableError::step_failed(
                        "compact_reconstruct",
                        e.to_string(),
                    )
                })?;
                // Yksi PENDING_PUT-rivi per elävä kirjaus, uudelleennumeroituna 0..N.
                let mut kept = Vec::with_capacity(state.len());
                let mut step = StepId::ZERO;
                for record in state.values() {
                    let payload = serde_json::to_value(record)?;
                    kept.push(JournalEntry::marker(step, PENDING_PUT, payload));
                    step = step.next();
                }
                live_count.set(kept.len());
                Ok(kept)
            })
            .map_err(|e| ActionError::Proof(format!("compact pending journal failed: {e}")))?;

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
    /// [`AUTO_COMPACT_MIN_ROWS`] JA `rivit > factor * elävät`. Muuten ei tee
    /// mitään. Kutsutaan lisäyksen ja häädön jälkeen, jotta kuolleet rivit eivät
    /// kerry rajatta. Auto-tiivistyksen epäonnistuminen **ei** kaada kutsujaa:
    /// data on jo turvallisesti lokissa, joten tiivistys on pelkkä optimointi —
    /// virhe niellään (loki vain pysyy tiivistämättömänä tällä kertaa).
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
            // Tiivistä; virhe niellään (data on jo lokissa, tiivistys on optimointi).
            let _ = self.compact();
        }
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
        self.append_marker(PENDING_PUT, payload)?;
        // Korvaus jätti kuolleen rivin (vanha put) → harkitse auto-tiivistystä.
        self.maybe_auto_compact();
        Ok(())
    }

    fn get(&self, approval_id: ApprovalId) -> Result<Option<PendingRecord>> {
        Ok(self.replay_state()?.remove(&approval_id))
    }

    fn remove(&self, approval_id: ApprovalId) -> Result<Option<PendingRecord>> {
        let existing = self.replay_state()?.remove(&approval_id);
        if existing.is_some() {
            // Kirjaa tombstone vain jos kirjaus oli olemassa — turha rivi vältetään.
            let payload = serde_json::to_value(approval_id)
                .map_err(|e| ActionError::Proof(format!("encode pending delete id failed: {e}")))?;
            self.append_marker(PENDING_DELETE, payload)?;
            // Tombstone on kuollut rivi → harkitse auto-tiivistystä.
            self.maybe_auto_compact();
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
            let payload = serde_json::to_value(id)
                .map_err(|e| ActionError::Proof(format!("encode pending delete id failed: {e}")))?;
            self.append_marker(PENDING_DELETE, payload)?;
        }
        if !expired.is_empty() {
            // Häätö tuotti tombstoneja (kuolleita rivejä) → harkitse tiivistystä.
            self.maybe_auto_compact();
        }
        Ok(expired.len())
    }

    /// Kaatumiskestävä pinta: `"journal"`.
    fn kind(&self) -> &'static str {
        "journal"
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
        let payload =
            serde_json::to_vec(&serde_json::json!({ "to": "general" })).expect("serialize payload");
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
        let store = JournalPendingStore::open_with_capacity(tmp.path(), PendingCapacity::new(1))
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

    // ---- Compaction ----

    /// Laskee fyysiset (eläviä + kuolleita) journal-rivit lukemalla tiedoston.
    fn physical_rows(path: &Path) -> usize {
        std::fs::read_to_string(path)
            .map_or(0, |s| s.lines().filter(|l| !l.trim().is_empty()).count())
    }

    #[test]
    fn compact_drops_dead_rows_keeps_live_entries() {
        let tmp = TempPath::new("compact-basic");
        let now = at(1_700_000_000);
        // Auto-tiivistys pois päältä, jotta hallitaan tiivistys käsin.
        let store = JournalPendingStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        // Kirjaa N kirjausta, poista puolet → kuolleita rivejä kertyy.
        let mut ids = Vec::new();
        for _ in 0..10 {
            let record = record_at(now, Duration::minutes(60));
            ids.push(record.approval_id());
            store.insert(record).expect("insert");
        }
        // Poista ensimmäiset 5 (10 put + 5 delete = 15 fyysistä riviä).
        for id in ids.iter().take(5) {
            store.remove(*id).expect("remove");
        }
        assert_eq!(physical_rows(tmp.path()), 15, "10 put + 5 tombstone");
        assert_eq!(store.len().expect("len"), 5, "5 live remain");

        // Tiivistä: 10 kuollutta riviä (5 poistettua put + 5 tombstone) pudotetaan.
        let dropped = store.compact().expect("compact");
        assert_eq!(dropped, 10, "15 rows → 5 live rows = 10 dropped");
        assert_eq!(
            physical_rows(tmp.path()),
            5,
            "only live rows remain on disk"
        );
        assert_eq!(store.len().expect("len"), 5, "live count unchanged");

        // Kaikki elävät kirjaukset yhä haettavissa, poistetut eivät.
        for id in ids.iter().take(5) {
            assert!(store.get(*id).expect("get").is_none(), "removed gone");
        }
        for id in ids.iter().skip(5) {
            assert!(store.get(*id).expect("get").is_some(), "live still present");
        }
    }

    #[test]
    fn compact_preserves_exact_state_across_reload() {
        let tmp = TempPath::new("compact-reload");
        let now = at(1_700_000_000);

        let live_ids = {
            let store = JournalPendingStore::open(tmp.path())
                .expect("open 1")
                .with_auto_compact_factor(0);
            let mut ids = Vec::new();
            for _ in 0..6 {
                let record = record_at(now, Duration::minutes(60));
                ids.push(record.approval_id());
                store.insert(record).expect("insert");
            }
            // Poista kolme.
            for id in ids.iter().take(3) {
                store.remove(*id).expect("remove");
            }
            store.compact().expect("compact");
            ids
        };

        // Restart pelkästä tiivistetystä tiedostosta → identtinen tila.
        let resumed = JournalPendingStore::open(tmp.path()).expect("open 2");
        assert_eq!(
            resumed.len().expect("len"),
            3,
            "3 live survive compaction+reload"
        );
        for id in live_ids.iter().take(3) {
            assert!(
                resumed.get(*id).expect("get").is_none(),
                "removed stay removed"
            );
        }
        for id in live_ids.iter().skip(3) {
            assert!(resumed.get(*id).expect("get").is_some(), "live stay live");
        }
        // Vain elävät rivit levyllä.
        assert_eq!(physical_rows(tmp.path()), 3);
    }

    #[test]
    fn compact_is_atomic_temp_then_rename() {
        // Tiivistys EI saa jättää temp-tiedostoa lojumaan eikä turmella elävää
        // tiedostoa: rewrite kirjoittaa temppiin, fsyncaa, ja vasta sitten
        // nimeää atomisesti. Jokainen rivi tiivistyksen jälkeen on ehjä JSON.
        let tmp = TempPath::new("compact-atomic");
        let now = at(1_700_000_000);
        let store = JournalPendingStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        let mut ids = Vec::new();
        for _ in 0..8 {
            let record = record_at(now, Duration::minutes(60));
            ids.push(record.approval_id());
            store.insert(record).expect("insert");
        }
        for id in &ids {
            store.remove(*id).expect("remove");
        }
        // Lisää yksi elävä takaisin.
        let live = record_at(now, Duration::minutes(60));
        let live_id = live.approval_id();
        store.insert(live).expect("insert live");

        store.compact().expect("compact");

        // Jokainen levyllä oleva rivi jäsentyy ehjäksi (ei puolikasta renamesta).
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read");
        for line in on_disk.lines().filter(|l| !l.trim().is_empty()) {
            serde_json::from_str::<JournalEntry>(line).expect("intact json line");
        }
        // Vain elävä kirjaus jäljellä, haettavissa.
        assert_eq!(store.len().expect("len"), 1);
        assert!(store.get(live_id).expect("get").is_some());

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
    fn compact_drops_expired_entries() {
        // Tiivistys EI itse häädä vanhentuneita, mutta evict_expired tombstonaa
        // ne ja sitä seuraava tiivistys pudottaa sekä tombstonet että kuolleet
        // put-rivit — joten vanhentuneet katoavat levyltä tiivistyksessä.
        let tmp = TempPath::new("compact-expired");
        let now = at(1_700_000_000);
        let store = JournalPendingStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        let short = record_at(now, Duration::seconds(60));
        let short_id = short.approval_id();
        let long = record_at(now, Duration::seconds(3600));
        let long_id = long.approval_id();
        store.insert(short).expect("insert short");
        store.insert(long).expect("insert long");

        // Häädä vanhentunut → tombstone. Sitten tiivistä.
        assert_eq!(store.evict_expired(at(1_700_000_120)).expect("evict"), 1);
        store.compact().expect("compact");

        // Vanhentunut on poissa sekä tilasta että levyltä; voimassa oleva säilyy.
        assert!(store.get(short_id).expect("get").is_none());
        assert!(store.get(long_id).expect("get").is_some());
        assert_eq!(physical_rows(tmp.path()), 1, "only the live entry remains");
        // Vanhentuneen tiivistetiiviste/tunniste ei näy enää levyllä.
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read");
        assert!(
            !on_disk.contains(&short_id.to_string()),
            "expired id gone from disk"
        );
    }

    #[test]
    fn auto_compact_triggers_when_dead_rows_exceed_threshold() {
        // Oletuskerroin (2) + insert/remove-sykli kasvattaa kuolleita rivejä,
        // kunnes auto-tiivistys laukeaa ja kutistaa lokin elävän tilan tasolle.
        let tmp = TempPath::new("auto-compact");
        let now = at(1_700_000_000);
        let store = JournalPendingStore::open(tmp.path()).expect("open"); // default factor

        // Pidä vain muutama elävä, mutta tee paljon insert/remove-pareja → kun
        // fyysisiä rivejä > 2*elävät JA >= AUTO_COMPACT_MIN_ROWS, tiivistys laukeaa.
        // Yksi pysyvä elävä:
        let keeper = record_at(now, Duration::minutes(60));
        let keeper_id = keeper.approval_id();
        store.insert(keeper).expect("insert keeper");

        // 100 insert+remove paria = 200 kuollutta riviä jos ei tiivistystä.
        for _ in 0..100 {
            let r = record_at(now, Duration::minutes(60));
            let id = r.approval_id();
            store.insert(r).expect("insert churn");
            store.remove(id).expect("remove churn");
        }

        // Auto-tiivistyksen ansiosta fyysisiä rivejä on PALJON vähemmän kuin 201.
        let rows = physical_rows(tmp.path());
        assert!(
            rows < 50,
            "auto-compaction should keep the log small, got {rows} rows"
        );
        // Elävä keeper säilyi koko ajan.
        assert!(store.get(keeper_id).expect("get").is_some());
        assert_eq!(store.len().expect("len"), 1);
    }

    #[test]
    fn compact_on_empty_log_is_noop() {
        let tmp = TempPath::new("compact-empty");
        let store = JournalPendingStore::open(tmp.path()).expect("open");
        assert_eq!(store.compact().expect("compact"), 0);
        assert!(store.is_empty().expect("empty"));
        assert_eq!(physical_rows(tmp.path()), 0);
    }

    /// TOCTOU-aukon sulkemisen regressio: tiivistys lukee tilan ja kirjoittaa
    /// lokin uudelleen **saman file-lukon alla** ([`FileJournal::compact_with`]),
    /// joten rinnakkainen lisäys ei voi laskeutua aukkoon ja kadota. Tässä ei aja
    /// oikeaa rinnakkaisuutta (race on epädeterministinen) — todistetaan sen
    /// sijaan rakenteesta seuraava havaittava invariantti: tiivistyksen PALUUN
    /// JÄLKEEN tehty lisäys laskeutuu tiivistettyjen elävien rivien PERÄÄN, ja
    /// uudelleenlataus tuottaa täsmälleen oikean tilan (sekä tiivistetyt elävät
    /// ETTÄ tiivistyksen jälkeen lisätty). Concurrent-append-turvallisuus seuraa
    /// nyt yhden-lukon-pidosta, ei vapaaehtoisesta ajoituksesta.
    #[test]
    fn compact_then_append_does_not_lose_post_compact_insert() {
        let tmp = TempPath::new("compact-toctou");
        let now = at(1_700_000_000);
        let store = JournalPendingStore::open(tmp.path())
            .expect("open")
            .with_auto_compact_factor(0);

        // Kuusi lisäystä, poista kolme → kuolleita rivejä kertyy.
        let mut ids = Vec::new();
        for _ in 0..6 {
            let record = record_at(now, Duration::minutes(60));
            ids.push(record.approval_id());
            store.insert(record).expect("insert");
        }
        for id in ids.iter().take(3) {
            store.remove(*id).expect("remove");
        }
        assert_eq!(store.len().expect("len"), 3, "3 live before compact");

        // Tiivistä (atominen, yhden lukon alla).
        let dropped = store.compact().expect("compact");
        assert_eq!(
            dropped, 6,
            "9 rows (6 put + 3 tombstone) → 3 live = 6 dropped"
        );
        assert_eq!(physical_rows(tmp.path()), 3, "only live rows after compact");

        // Tiivistyksen JÄLKEEN lisätty kirjaus laskeutuu elävien PERÄÄN (ei katoa).
        let post = record_at(now, Duration::minutes(60));
        let post_id = post.approval_id();
        store.insert(post).expect("insert after compact");
        assert_eq!(store.len().expect("len"), 4, "3 live + 1 post-compact");
        assert!(
            store.get(post_id).expect("get").is_some(),
            "post-compact insert present"
        );

        // Uudelleenlataus tuottaa TÄSMÄLLEEN oikean tilan: tiivistetyt elävät +
        // tiivistyksen jälkeen lisätty; poistetut pysyvät poissa.
        let resumed = JournalPendingStore::open(tmp.path()).expect("reopen");
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
        limiter
            .check_and_record("being-a", now)
            .expect("being-a first");
        // Eri olento → oma kiintiö.
        limiter
            .check_and_record("being-b", now)
            .expect("being-b first");
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
