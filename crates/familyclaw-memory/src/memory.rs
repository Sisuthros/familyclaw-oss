//! Yksittäinen muisto ([`Memory`]) ja sen elinkaaritila ([`MemoryStatus`]).
//!
//! `Memory` on Eternal Threadin perusyksikkö: sisältö, tunnemerkintä
//! ([`Vad`] + nimetyt [`Dimension`]-tunteet), tärkeys, vaimennuspolitiikka
//! ja elinkaaritila. Muistot luodaan [`MemoryBuilder`]-rakentajalla ja
//! tallennetaan [`crate::MemoryStore`]-toteutukseen.

use familyclaw_core::{time, MessageId, Timestamp};
use familyclaw_emotion::{Dimension, Vad};
use serde::{Deserialize, Serialize};

use crate::decay::DecayPolicy;
use crate::importance::ImportanceFactors;

/// Muistin vahvuuden (`S`) ala- ja yläraja, kun se johdetaan tärkeydestä.
///
/// Neutraalikin muisto saa perussäilyvyyden ([`STABILITY_MIN`]); maksimi-
/// tärkeä muisto venyy [`STABILITY_MAX`]:iin. Arvot ovat
/// [`crate::decay`]-moduulin aikaskaalan (1.0 ≈ vuorokausi) yksiköissä.
pub const STABILITY_MIN: f32 = 0.5;
/// Muistin vahvuuden yläraja (kts. [`STABILITY_MIN`]).
pub const STABILITY_MAX: f32 = 8.0;

/// Muiston elinkaaritila.
///
/// Tila siirtyy yksisuuntaisesti: `Active → Archived → Tombstoned`.
/// Arkistoitu muisto on yhä haettavissa (heikennettynä), mutta haudattu
/// (tombstoned) on poistettu aktiivisesta haetusta ja odottaa lopullista
/// siivousta (design §5: status-elinkaari Active/Archived/Tombstoned).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    /// Aktiivinen — täysipainoinen muisto, mukana haussa. (Oletustila.)
    #[default]
    Active,
    /// Arkistoitu — vaimentunut mutta yhä haettavissa heikennettynä.
    Archived,
    /// Haudattu — poistettu aktiivisesta haetusta, odottaa siivousta.
    Tombstoned,
}

impl MemoryStatus {
    /// Onko muisto vielä haettavissa (aktiivinen tai arkistoitu).
    #[must_use]
    pub const fn is_retrievable(self) -> bool {
        matches!(self, MemoryStatus::Active | MemoryStatus::Archived)
    }

    /// Vakaa, kone-luettava nimi (`snake_case`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MemoryStatus::Active => "active",
            MemoryStatus::Archived => "archived",
            MemoryStatus::Tombstoned => "tombstoned",
        }
    }
}

impl std::fmt::Display for MemoryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// verification-gated verification-gated memory
// ---------------------------------------------------------------------------

/// Muiston varmennustila: kuinka luotettava tämä tieto on.
///
/// Uusi muisto on aina `Claim` — väite ilman todisteita. Kun todisteita
/// kertyy, se nousee `Evidence`-tasolle ja lopulta `Confirmed`-tasolle
/// (jossa sillä on vähintään kaksi eri todistetyyppiä).
///
/// Tämä on ortogonaalinen elinkaaritilaan (`MemoryStatus`) nähden: muisto
/// voi olla `Active` ja `Claim` yhtä aikaa. Confirmed-muisto unohtuu
/// hitaammin retrieval-painotuksessa (confidence × retention), mutta
/// elinkaaritila (`Active → Archived → Tombstoned`) toimii samoin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    /// Väite — ei vahvistettu, voi olla väärä. (Oletustila uusille muistoille.)
    #[default]
    Claim,
    /// Todisteita on olemassa (vähintään yksi), mutta ei vielä varmistettu.
    Evidence,
    /// Vahvistettu vähintään kahdella eri todistetyypillä.
    Confirmed,
}

impl VerificationStatus {
    /// Palauttaa painon (0.0–1.0) Oracle-scoringia ja retrieval-painotusta varten.
    #[must_use]
    pub const fn weight(self) -> f32 {
        match self {
            VerificationStatus::Claim => 0.2,
            VerificationStatus::Evidence => 0.6,
            VerificationStatus::Confirmed => 1.0,
        }
    }
}

impl std::fmt::Display for VerificationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            VerificationStatus::Claim => "claim",
            VerificationStatus::Evidence => "evidence",
            VerificationStatus::Confirmed => "confirmed",
        })
    }
}

/// Todistetyyppi muiston varmennukseen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    /// Build meni läpi.
    BuildPassed,
    /// Testit meni läpi.
    TestPassed,
    /// Käyttäjä vahvisti.
    UserConfirmation,
    /// Riippumaton havainto (toinen agentti vahvisti).
    IndependentObservation,
    /// Ulkoinen dokumentaatio vahvistaa.
    ExternalDoc,
    /// Tuotantometriikka vahvistaa.
    ProductionMetric,
}

impl std::fmt::Display for EvidenceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EvidenceType::BuildPassed => "build_passed",
            EvidenceType::TestPassed => "test_passed",
            EvidenceType::UserConfirmation => "user_confirmation",
            EvidenceType::IndependentObservation => "independent_observation",
            EvidenceType::ExternalDoc => "external_doc",
            EvidenceType::ProductionMetric => "production_metric",
        })
    }
}

/// Yksittäinen todiste muiston tueksi.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// Todistetyyppi.
    pub evidence_type: EvidenceType,
    /// Linkki todisteeseen (commit SHA, testinimi, keskustelu-id tms.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Aikaleima.
    pub recorded_at: Timestamp,
}

impl Evidence {
    /// Luo uuden todisteen.
    #[must_use]
    pub fn new(evidence_type: EvidenceType, link: Option<String>) -> Self {
        Self {
            evidence_type,
            link,
            recorded_at: familyclaw_core::time::now(),
        }
    }
}

/// Yksittäinen Eternal Thread -muisto.
///
/// Luo muisto [`Memory::builder`]-rakentajalla. Kentät ovat julkisia
/// lukemista varten, mutta käytä mutaatiometodeja
/// ([`reinforce`](Memory::reinforce), [`archive`](Memory::archive),
/// [`tombstone`](Memory::tombstone)) jotta johdetut arvot (tärkeys,
/// vahvistuslaskuri) pysyvät johdonmukaisina.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    /// Muiston yksilöivä tunniste.
    pub id: MessageId,

    /// Muiston tekstisisältö.
    pub content: String,

    /// Matala-ulotteinen VAD-yhteenveto muiston tunnesävystä.
    pub vad: Vad,

    /// Nimetyt tunnedimensiot jotka muisto aktivoi (esim. `Gratitude`).
    #[serde(default)]
    pub emotions: Vec<Dimension>,

    /// Luontihetki (UTC).
    pub created_at: Timestamp,

    /// Viimeisin aktivointi/vahvistus (UTC) — käytetään retentiolaskennan
    /// aikaperustana. Alussa sama kuin [`created_at`](Memory::created_at).
    pub last_reinforced_at: Timestamp,

    /// Esilaskettu yhdistelmätärkeys, `0.0..=1.0`.
    pub importance: f32,

    /// Tärkeyden osatekijät, joista [`importance`](Memory::importance)
    /// johdetaan (säilytetään uudelleenlaskentaa ja diagnostiikkaa varten).
    pub factors: ImportanceFactors,

    /// Vaimennuspolitiikka (Ebbinghaus λ).
    pub decay_policy: DecayPolicy,

    /// Kuinka monta kertaa muisto on vahvistettu (luonti = 0).
    #[serde(default)]
    pub reinforcement_count: u32,

    /// Vapaamuotoiset luokittelutägit (geneerisiä — ei kovakoodattua
    /// perhe-/avain-/polkutietoa).
    #[serde(default)]
    pub tags: Vec<String>,

    /// Muiston lähde (esim. `"chat"`, `"reflection"`).
    #[serde(default)]
    pub source: String,

    /// Elinkaaritila.
    #[serde(default)]
    pub status: MemoryStatus,

    /// Deterministinen dedupointiavain (agentin turn-numero + tunniste).
    /// Jos asetettu, `MemoryStore::add` ohittaa jo kirjatun saman avaimen
    /// muiston, jolloin muistikirjaus on idempotentti replayssa
    /// (ratkaisee dual-write-ongelman: durable.step onnistuu mutta
    /// `memory_store.add` ei ehdi ennen kaatumista).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_key: Option<String>,

    /// Valinnainen upotusvektori semanttista haun varten.
    /// Jos asetettu, haku voi käyttää cosine-similarityä avainsanan sijaan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,

    // ── verification-gated -kentät ──────────────────────────────────────────
    // Kaikki #[serde(default)] — taaksepäin yhteensopiva olemassaolevien
    // persistoitujen muistojen kanssa (vanha JSON ilman näitä kenttiä
    // deserialisoituu oikein oletusarvoilla).
    /// Varmennustila — kuinka luotettava tämä muisto on.
    /// Uusi muisto on aina `Claim` (varmistamaton väite).
    #[serde(default)]
    pub verification_status: VerificationStatus,

    /// Luottamustaso 0.0–1.0, johdettu varmennustilasta ja evidenceistä.
    /// Käytetään Oracle-scoringissa ja retrieval-painotuksessa.
    #[serde(default)]
    pub confidence: f32,

    /// Todisteet jotka tukevat tätä muistoa.
    /// Tyhjä = ei todisteita (Claim-tason muisto).
    #[serde(default)]
    pub evidence: Vec<Evidence>,

    /// Ryhmittelyavain samankaltaisille muistoille (esim. `"db-valinta"`,
    /// `"provider-prefix-bug"`). Käytetään Oracle-preflightissa
    /// frekvenssilaskentaan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_key: Option<String>,
}

impl Memory {
    /// Aloittaa uuden muiston rakentamisen annetulla sisällöllä.
    #[must_use]
    pub fn builder(content: impl Into<String>) -> MemoryBuilder {
        MemoryBuilder::new(content)
    }

    /// Muiston ikä sekunteina suhteessa annettuun hetkeen, viimeisestä
    /// vahvistuksesta laskettuna. Negatiivinen erotus (kello taaksepäin)
    /// palautetaan nollana.
    ///
    /// Tarkkuus on sekunnin luokkaa: alle-sekunnin osa pyöristyy pois, mikä
    /// riittää eksponentiaaliseen unohtamiskäyrään.
    #[must_use]
    pub fn age_secs(&self, at: Timestamp) -> f32 {
        let delta = at.signed_duration_since(self.last_reinforced_at);
        let secs = delta.num_seconds();
        if secs <= 0 {
            return 0.0;
        }
        // i64-sekunnit → f32: tarkkuushäviö on hyväksyttävää (retentio on jo
        // approksimaatio, eikä sekuntitason heitto vuosien skaalalla muuta
        // unohtamiskäyrää). i64-arvo ei vuoda mantissan rajojen yli
        // realistisilla aikaväleillä.
        #[allow(clippy::cast_precision_loss)]
        let result = secs as f32;
        result
    }

    /// Muiston nykyinen retentio (`0.0..=1.0`) ajanhetkellä `at`.
    ///
    /// Yhdistää vaimennuspolitiikan ([`decay_policy`](Memory::decay_policy))
    /// ja tärkeydestä johdetun vahvuuden ([`stability`](Memory::stability)).
    /// Suojattu ydin palauttaa aina `1.0`.
    #[must_use]
    pub fn retention(&self, at: Timestamp) -> f32 {
        self.decay_policy
            .retention(self.age_secs(at), self.stability())
    }

    /// Muiston vahvuus `S` Ebbinghaus-kaavaan, johdettuna tärkeydestä.
    #[must_use]
    pub fn stability(&self) -> f32 {
        self.factors.stability(STABILITY_MIN, STABILITY_MAX)
    }

    /// Onko muisto vielä haettavissa (tila aktiivinen/arkistoitu).
    #[must_use]
    pub fn is_retrievable(&self) -> bool {
        self.status.is_retrievable()
    }

    /// Vahvistaa muiston: nostaa vahvistuslaskuria, päivittää
    /// aikaperustan hetkeen `at` ja laskee tärkeyden uudelleen
    /// päivitetyllä reinforcement-osatekijällä.
    ///
    /// Vahvistusosatekijä kasvaa kyllästyvästi (`1 - e^(-count/3)`), joten
    /// toistuva aktivointi nostaa säilyvyyttä mutta kyllästyy — yksi muisto
    /// ei voi kaapata koko tärkeysasteikkoa pelkällä toistolla.
    pub fn reinforce(&mut self, at: Timestamp) {
        self.reinforcement_count = self.reinforcement_count.saturating_add(1);
        self.last_reinforced_at = at;
        #[allow(clippy::cast_precision_loss)]
        let count = self.reinforcement_count as f32;
        let reinforcement = 1.0 - (-count / 3.0).exp();
        self.factors.reinforcement = reinforcement.clamp(0.0, 1.0);
        self.importance = self.factors.composite();
        // Vahvistus voi elvyttää arkistoidun takaisin aktiiviseksi.
        if self.status == MemoryStatus::Archived {
            self.status = MemoryStatus::Active;
        }
    }

    /// Siirtää muiston arkistoon (jos se ei ole jo haudattu).
    ///
    /// Palauttaa `true` jos tila muuttui. Haudattua muistoa ei voi
    /// arkistoida takaisin.
    pub fn archive(&mut self) -> bool {
        if self.status == MemoryStatus::Active {
            self.status = MemoryStatus::Archived;
            true
        } else {
            false
        }
    }

    /// Hautaa muiston (tombstone) — poistaa sen aktiivisesta haetusta.
    ///
    /// Suojattua ydintä ([`DecayPolicy::ProtectedCore`]) **ei voi haudata**:
    /// metodi palauttaa silloin `false` eikä muuta tilaa. Muutoin palauttaa
    /// `true` jos tila muuttui.
    pub fn tombstone(&mut self) -> bool {
        if self.decay_policy.is_protected() {
            return false;
        }
        if self.status == MemoryStatus::Tombstoned {
            false
        } else {
            self.status = MemoryStatus::Tombstoned;
            true
        }
    }

    // ── verification-gated varmennusmetodit ───────────────────────────────

    /// Lisää todisteen ja päivittää varmennustilan automaattisesti.
    ///
    /// # Promote-säännöt
    /// - `Claim` + 1 evidence (mikä tahansa) → `Evidence` (confidence 0.7)
    /// - `Evidence` + `UserConfirmation` → `Confirmed` (confidence 1.0)
    /// - `Claim` + 2 distinct evidence types → `Confirmed` (confidence 1.0)
    /// - `Confirmed` pysyy `Confirmed` — confidence ei laske koskaan.
    pub fn add_evidence(&mut self, evidence: Evidence) {
        self.evidence.push(evidence);

        // Kerää uniikit todistetyypit
        let mut types: Vec<EvidenceType> = self.evidence.iter().map(|e| e.evidence_type).collect();
        types.sort();
        types.dedup();

        match self.verification_status {
            VerificationStatus::Claim => {
                if types.len() >= 2 {
                    self.verification_status = VerificationStatus::Confirmed;
                    self.confidence = 1.0;
                } else {
                    self.verification_status = VerificationStatus::Evidence;
                    self.confidence = 0.7;
                }
            }
            VerificationStatus::Evidence => {
                if types.contains(&EvidenceType::UserConfirmation) || types.len() >= 2 {
                    self.verification_status = VerificationStatus::Confirmed;
                    self.confidence = 1.0;
                }
                // Muuten pysyy Evidencenä — yksi todiste ei riitä
            }
            VerificationStatus::Confirmed => {
                // Confirmed pysyy confirmed — confidence voi nousta, muttei laske
                self.confidence = self.confidence.max(1.0);
            }
        }
    }

    /// Onko muisto vahvistettu (luotettava)?
    #[must_use]
    pub const fn is_confirmed(&self) -> bool {
        matches!(self.verification_status, VerificationStatus::Confirmed)
    }
}

/// [`Memory`]-rakentaja, joka asettaa johdetut kentät (tärkeys, aikaleimat,
/// vahvuus) johdonmukaisesti.
///
/// Hanki rakentaja [`Memory::builder`]-metodilla, säädä kentät builder-
/// tyylillä ja viimeistele [`MemoryBuilder::build`]-metodilla.
#[derive(Debug, Clone)]
pub struct MemoryBuilder {
    content: String,
    vad: Vad,
    emotions: Vec<Dimension>,
    created_at: Timestamp,
    factors: ImportanceFactors,
    decay_policy: DecayPolicy,
    tags: Vec<String>,
    source: String,
    turn_key: Option<String>,
    embedding: Option<Vec<f32>>,
    // verification-gated -kentät
    verification_status: VerificationStatus,
    evidence: Vec<Evidence>,
    pattern_key: Option<String>,
}

impl MemoryBuilder {
    /// Luo rakentajan sisällöllä; muut kentät saavat neutraalit oletukset.
    #[must_use]
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            vad: Vad::NEUTRAL,
            emotions: Vec::new(),
            created_at: time::now(),
            factors: ImportanceFactors::ZERO,
            decay_policy: DecayPolicy::Normal,
            tags: Vec::new(),
            source: String::new(),
            turn_key: None,
            // verification-gated -oletukset: uusi muisto alkaa varmistamattomana väitteenä
            verification_status: VerificationStatus::Claim,
            embedding: None,
            evidence: Vec::new(),
            pattern_key: None,
        }
    }

    /// Asettaa VAD-yhteenvedon.
    #[must_use]
    pub fn vad(mut self, vad: Vad) -> Self {
        self.vad = vad;
        self
    }

    /// Asettaa nimetyt tunnedimensiot.
    #[must_use]
    pub fn emotions(mut self, emotions: impl IntoIterator<Item = Dimension>) -> Self {
        self.emotions = emotions.into_iter().collect();
        self
    }

    /// Asettaa tärkeyden osatekijät.
    #[must_use]
    pub fn factors(mut self, factors: ImportanceFactors) -> Self {
        self.factors = factors;
        self
    }

    /// Asettaa vaimennuspolitiikan.
    #[must_use]
    pub fn decay_policy(mut self, policy: DecayPolicy) -> Self {
        self.decay_policy = policy;
        self
    }

    /// Ohittaa luontihetken (oletus: nyt). Käytännöllinen testeissä ja
    /// datan migraatiossa.
    #[must_use]
    pub fn created_at(mut self, at: Timestamp) -> Self {
        self.created_at = at;
        self
    }

    /// Asettaa luokittelutägit.
    #[must_use]
    pub fn tags(mut self, tags: impl IntoIterator<Item = String>) -> Self {
        self.tags = tags.into_iter().collect();
        self
    }

    /// Asettaa lähteen.
    #[must_use]
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// Asettaa varmennustilan (oletus: `Claim`).
    #[must_use]
    pub fn verification_status(mut self, status: VerificationStatus) -> Self {
        self.verification_status = status;
        self
    }

    /// Asettaa ryhmittelyavaimen (`pattern_key`) oraclen frekvenssilaskentaa varten.
    #[must_use]
    pub fn pattern_key(mut self, key: impl Into<String>) -> Self {
        self.pattern_key = Some(key.into());
        self
    }

    /// Asettaa upotusvektorin semanttista haun varten.
    #[must_use]
    pub fn embedding(mut self, embedding: impl Into<Vec<f32>>) -> Self {
        self.embedding = Some(embedding.into());
        self
    }

    /// Viimeistelee muiston: generoi tunnisteen, asettaa aikaleimat ja
    /// laskee tärkeyden osatekijöistä. Tila on aina [`MemoryStatus::Active`].
    #[must_use]
    pub fn build(self) -> Memory {
        let importance = self.factors.composite();
        Memory {
            id: MessageId::new(),
            content: self.content,
            vad: self.vad,
            emotions: self.emotions,
            created_at: self.created_at,
            last_reinforced_at: self.created_at,
            importance,
            factors: self.factors,
            decay_policy: self.decay_policy,
            reinforcement_count: 0,
            tags: self.tags,
            source: self.source,
            status: MemoryStatus::Active,
            turn_key: self.turn_key,
            embedding: self.embedding,
            verification_status: self.verification_status,
            confidence: 0.0, // Asetetaan promote-logiikalla add_evidence()-kutsujen kautta
            evidence: self.evidence,
            pattern_key: self.pattern_key,
        }
    }
}

#[cfg(test)]
mod tests {
    // Testit vertaavat tarkasti esitettäviä f32-vakioita — tarkka vertailu ok.
    #![allow(clippy::float_cmp)]

    use super::*;
    use chrono::Duration;
    use familyclaw_core::time;

    fn warm_factors() -> ImportanceFactors {
        ImportanceFactors::new(0.8, 0.4, 0.2, 0.0)
    }

    #[test]
    fn builder_sets_fields_and_derives_importance() {
        let m = Memory::builder("hei maailma")
            .vad(Vad::new(0.5, 0.4, 0.6))
            .emotions([Dimension::Joy, Dimension::Curiosity])
            .factors(warm_factors())
            .decay_policy(DecayPolicy::Slow)
            .tags(["greeting".to_string()])
            .source("chat")
            .build();

        assert_eq!(m.content, "hei maailma");
        assert_eq!(m.emotions, vec![Dimension::Joy, Dimension::Curiosity]);
        assert_eq!(m.decay_policy, DecayPolicy::Slow);
        assert_eq!(m.status, MemoryStatus::Active);
        assert_eq!(m.reinforcement_count, 0);
        assert_eq!(m.source, "chat");
        assert!((m.importance - warm_factors().composite()).abs() < 1e-6);
        // Luonnissa aikaleimat ovat samat.
        assert_eq!(m.created_at, m.last_reinforced_at);
        assert!(!m.id.is_nil());
    }

    #[test]
    fn fresh_memory_has_full_retention() {
        let m = Memory::builder("tuore").factors(warm_factors()).build();
        let r = m.retention(m.created_at);
        assert!((r - 1.0).abs() < 1e-6);
    }

    #[test]
    fn retention_drops_over_time() {
        let created = time::now();
        let m = Memory::builder("vanheneva")
            .factors(ImportanceFactors::new(0.2, 0.0, 0.0, 0.0))
            .decay_policy(DecayPolicy::Normal)
            .created_at(created)
            .build();
        let later = created + Duration::days(7);
        let r = m.retention(later);
        assert!(r < 1.0);
        assert!(r > 0.0);
    }

    #[test]
    fn protected_core_keeps_full_retention_forever() {
        let created = time::now();
        let m = Memory::builder("minä olen")
            .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
            .decay_policy(DecayPolicy::ProtectedCore)
            .created_at(created)
            .build();
        let far_future = created + Duration::days(3650);
        assert_eq!(m.retention(far_future), 1.0);
    }

    #[test]
    fn higher_importance_retains_longer() {
        let created = time::now();
        let weak = Memory::builder("heikko")
            .factors(ImportanceFactors::new(0.1, 0.0, 0.0, 0.0))
            .created_at(created)
            .build();
        let strong = Memory::builder("vahva")
            .factors(ImportanceFactors::new(1.0, 1.0, 1.0, 1.0))
            .created_at(created)
            .build();
        let later = created + Duration::days(10);
        assert!(strong.retention(later) > weak.retention(later));
    }

    #[test]
    fn age_secs_never_negative() {
        let created = time::now();
        let m = Memory::builder("x").created_at(created).build();
        // Kello taaksepäin → 0.
        let earlier = created - Duration::hours(1);
        assert_eq!(m.age_secs(earlier), 0.0);
        // Eteenpäin → positiivinen.
        let later = created + Duration::seconds(3600);
        assert!((m.age_secs(later) - 3600.0).abs() < 1.0);
    }

    #[test]
    fn reinforce_increases_count_and_importance() {
        let created = time::now();
        let mut m = Memory::builder("vahvistettava")
            .factors(ImportanceFactors::new(0.3, 0.0, 0.0, 0.0))
            .created_at(created)
            .build();
        let before = m.importance;
        let count_before = m.reinforcement_count;

        m.reinforce(created + Duration::hours(1));
        assert_eq!(m.reinforcement_count, count_before + 1);
        assert!(m.importance > before, "vahvistus ei nostanut tärkeyttä");
        assert!(m.factors.reinforcement > 0.0);
        // Aikaperusta päivittyi → muisto on jälleen tuore.
        assert!((m.retention(m.last_reinforced_at) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn reinforcement_saturates() {
        let created = time::now();
        let mut m = Memory::builder("toistuva").created_at(created).build();
        for _ in 0..100 {
            m.reinforce(created);
        }
        // Kyllästyy 1.0:aan, ei ylitä.
        assert!(m.factors.reinforcement <= 1.0);
        assert!(m.factors.reinforcement > 0.99);
    }

    #[test]
    fn archive_transition_only_from_active() {
        let mut m = Memory::builder("a").build();
        assert!(m.archive());
        assert_eq!(m.status, MemoryStatus::Archived);
        // Toistuva arkistointi ei tee mitään.
        assert!(!m.archive());
        assert!(m.is_retrievable());
    }

    #[test]
    fn reinforce_revives_archived() {
        let mut m = Memory::builder("a").build();
        m.archive();
        assert_eq!(m.status, MemoryStatus::Archived);
        m.reinforce(time::now());
        assert_eq!(m.status, MemoryStatus::Active);
    }

    #[test]
    fn tombstone_transitions_and_blocks_protected() {
        let mut m = Memory::builder("haudattava")
            .decay_policy(DecayPolicy::Fast)
            .build();
        assert!(m.tombstone());
        assert_eq!(m.status, MemoryStatus::Tombstoned);
        assert!(!m.is_retrievable());
        // Toistuva hautaus ei muuta tilaa.
        assert!(!m.tombstone());

        // Suojattua ydintä ei voi haudata.
        let mut core = Memory::builder("ydin")
            .decay_policy(DecayPolicy::ProtectedCore)
            .build();
        assert!(!core.tombstone());
        assert_eq!(core.status, MemoryStatus::Active);
    }

    #[test]
    fn status_helpers() {
        assert!(MemoryStatus::Active.is_retrievable());
        assert!(MemoryStatus::Archived.is_retrievable());
        assert!(!MemoryStatus::Tombstoned.is_retrievable());
        assert_eq!(MemoryStatus::default(), MemoryStatus::Active);
        assert_eq!(MemoryStatus::Tombstoned.to_string(), "tombstoned");
    }

    #[test]
    fn serde_roundtrip_preserves_memory() {
        let m = Memory::builder("sarjallistuva")
            .vad(Vad::new(0.2, 0.3, 0.5))
            .emotions([Dimension::Hope, Dimension::Trust])
            .factors(warm_factors())
            .decay_policy(DecayPolicy::Slow)
            .tags(["t1".to_string(), "t2".to_string()])
            .source("test")
            .build();
        let json = serde_json::to_string(&m).expect("serialize");
        let back: Memory = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(m, back);
    }

    #[test]
    fn status_serializes_snake_case() {
        let json = serde_json::to_string(&MemoryStatus::Tombstoned).expect("serialize");
        assert_eq!(json, "\"tombstoned\"");
    }
}
