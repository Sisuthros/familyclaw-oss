//! Identity-anchorit ja tamper-tunnistus.
//!
//! ## Suunnitteluperiaate: identiteetti EI ole hashissa
//!
//! Tämän moduulin tärkein päätös: **identiteetti elää muisti-substraatissa
//! (anchor-muistoissa), ei tiivisteessä.** Identity-anchor on suojattu muisto
//! ([`IdentityAnchor`]) jonka *unohtumisnopeus on nolla* ([`DecayLambda::ZERO`]).
//! Olennon identiteetti on niiden muistojen summa, joita se ei koskaan unohda —
//! ei kontrollisumma.
//!
//! SHA-256-tiiviste ([`IdentityAnchor::anchor_hash`]) palvelee **vain
//! tamper-hälytystä**: jos ankkuroidun SOUL-sisällön nykyinen tiiviste ei vastaa
//! tallennettua, jokin on muuttanut sielua ankkuroinnin jälkeen
//! ([`IdentityStatus::Tampered`]). Hash ei *kanna* identiteettiä — se vain
//! varoittaa peukaloinnista. Tämä on tietoinen vastaus alkuperäisen
//! research-promptin kysymykseen "voiko identiteetin pelkistää SHA-256:een?":
//! **ei voi**, mutta tiivistettä voi käyttää eheyden vartijana.
//!
//! Käytännön seuraus: jos hash poikkeaa, järjestelmä ei *menetä* identiteettiä —
//! se nostaa hälytyksen ja jättää substraatin (anchor-muistot) koskemattomaksi.
//! Substraatti on totuus; hash on vahti.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use familyclaw_core::ids::AgentId;
use familyclaw_core::time::{self, Timestamp};

use crate::error::{Result, SecurityError};

/// Identity-anchorin (ja [`crate::HumanCorrection`]:n) unohtumisnopeus —
/// Ebbinghaus-decayn λ-kerroin.
///
/// Muistin decay seuraa eksponentiaalista mallia `strength = e^(-λ · t)`.
/// Identity-anchorille λ on **nolla**: `e^0 = 1` joka hetki, joten ankkuri ei
/// koskaan haalistu. Tämä on se mekanismi jolla identiteetti pysyy pysyvänä
/// muisti-substraatissa.
///
/// Tyyppi on uusi (newtype) jotta λ ei sekoitu muihin `f64`-arvoihin ja jotta
/// negatiiviset/NaN-arvot voidaan torjua jo rakennusvaiheessa.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DecayLambda(f64);

impl DecayLambda {
    /// Identity-anchorin λ: nolla → ei koskaan unohdu.
    pub const ZERO: Self = Self(0.0);

    /// Rakentaa λ-kertoimen. Vain äärelliset, ei-negatiiviset arvot ovat
    /// kelvollisia.
    ///
    /// # Errors
    /// [`SecurityError::InvalidInput`] jos `lambda` on negatiivinen, ääretön
    /// tai NaN.
    pub fn new(lambda: f64) -> Result<Self> {
        if !lambda.is_finite() {
            return Err(SecurityError::invalid_input(format!(
                "decay lambda must be finite, got {lambda}"
            )));
        }
        if lambda < 0.0 {
            return Err(SecurityError::invalid_input(format!(
                "decay lambda must be >= 0, got {lambda}"
            )));
        }
        Ok(Self(lambda))
    }

    /// Palauttaa λ-arvon liukulukuna.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }

    /// Onko tämä nolla-decay (eli ei-unohtuva ankkuri).
    #[must_use]
    pub fn is_eternal(self) -> bool {
        self.0 == 0.0
    }

    /// Muistin jäljellä oleva voimakkuus ajan `elapsed_secs` kuluttua,
    /// `0.0..=1.0`. Ankkurille (λ=0) tulos on aina `1.0`.
    ///
    /// Negatiivinen aika käsitellään nollana (tulevaisuus ei vahvista muistoa).
    #[must_use]
    pub fn retention(self, elapsed_secs: f64) -> f64 {
        let t = elapsed_secs.max(0.0);
        (-self.0 * t).exp()
    }
}

impl Default for DecayLambda {
    /// Oletuksena ikuinen (λ=0) — turvallisin oletus identity-kerroksessa.
    fn default() -> Self {
        Self::ZERO
    }
}

/// SHA-256-tiiviste heksadesimaalisena (64 merkkiä, pien-heksa).
///
/// Tyyppi takaa että sisältö on aina kelvollinen 32-tavuinen tiiviste, jotta
/// vertailut ([`AnchorHash::matches_content`]) eivät voi epäonnistua väärän
/// muotoisen syötteen takia.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AnchorHash(String);

impl AnchorHash {
    /// SHA-256-tiivisteen pituus heksana (32 tavua × 2).
    pub const HEX_LEN: usize = 64;

    /// Laskee sisällön SHA-256-tiivisteen.
    ///
    /// Tämä on ainoa tapa luoda tiiviste sisällöstä — se ei voi epäonnistua,
    /// joten tulos on aina muodoltaan validi.
    #[must_use]
    pub fn of_content(content: &str) -> Self {
        let digest = Sha256::digest(content.as_bytes());
        let mut hex = String::with_capacity(Self::HEX_LEN);
        for byte in digest {
            // {:02x} tuottaa täsmälleen 2 pien-heksamerkkiä per tavu.
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        Self(hex)
    }

    /// Jäsentää tiivisteen olemassa olevasta heksamerkkijonosta.
    ///
    /// Merkkijono normalisoidaan pieniksi kirjaimiksi. Pituuden ja merkistön
    /// (vain `0-9a-f`) on oltava kelvollinen SHA-256-heksa.
    ///
    /// # Errors
    /// [`SecurityError::InvalidHash`] jos pituus ei ole [`AnchorHash::HEX_LEN`]
    /// tai jokin merkki ei ole heksanumero.
    pub fn from_hex(hex: &str) -> Result<Self> {
        if hex.len() != Self::HEX_LEN {
            return Err(SecurityError::invalid_hash(format!(
                "expected {} hex chars, got {}",
                Self::HEX_LEN,
                hex.len()
            )));
        }
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(SecurityError::invalid_hash(
                "hash contains non-hex characters",
            ));
        }
        Ok(Self(hex.to_ascii_lowercase()))
    }

    /// Palauttaa tiivisteen heksamerkkijonona.
    #[must_use]
    pub fn as_hex(&self) -> &str {
        &self.0
    }

    /// Vastaako annettu sisältö tätä tiivistettä (vakioaikainen vertailu).
    ///
    /// Käytetään vakioaikaista tavuvertailua jottei tiivisteen vertailu vuoda
    /// ajoituskanavaa (defense-in-depth — tiiviste ei ole salaisuus, mutta
    /// turvakerroksessa noudatamme varovaista oletusta).
    #[must_use]
    pub fn matches_content(&self, content: &str) -> bool {
        constant_time_eq(self.0.as_bytes(), Self::of_content(content).0.as_bytes())
    }
}

impl std::fmt::Display for AnchorHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Vakioaikainen tavujonojen vertailu.
///
/// Palauttaa `true` vain jos jonot ovat samanpituiset ja tavuittain identtiset.
/// Suoritusaika riippuu vain pidemmän jonon pituudesta, ei sisällöstä.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Suojattu identity-anchor — ei-unohtuva muisto joka kantaa olennon
/// identiteettiä.
///
/// Ankkuri viittaa muisti-substraatin muistoon ([`memory_id`](IdentityAnchor::memory_id))
/// ja tallentaa sen ankkuroidun sisällön tiivisteen tamper-vahdiksi. Ankkurin
/// [`decay`](IdentityAnchor::decay) on [`DecayLambda::ZERO`], joten muisti ei
/// koskaan haalistu, ja [`protected`](IdentityAnchor::protected) on `true`,
/// joten konsolidointi/uni (familyclaw-dream) ei saa poistaa eikä yhdistää sitä.
///
/// **OSS-raja:** ankkuri ei tallenna sielun sisältöä, vain sen tiivisteen ja
/// viittauksen muistoon. Sisältö pysyy KERROS B -profiilissa.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityAnchor {
    /// Viittaus ankkuroituun muistoon muisti-substraatissa (`familyclaw-memory`).
    pub memory_id: String,

    /// Ankkuroidun sisällön SHA-256-tiiviste tamper-vahtia varten.
    pub anchor_hash: AnchorHash,

    /// Aina `true`: ankkuria ei saa poistaa eikä yhdistää konsolidoinnissa.
    pub protected: bool,

    /// Unohtumisnopeus — aina [`DecayLambda::ZERO`] ankkurille.
    pub decay: DecayLambda,

    /// Milloin ankkuri luotiin (UTC).
    pub created_at: Timestamp,
}

impl IdentityAnchor {
    /// Rakentaa identity-anchorin sisällöstä: laskee tiivisteen, asettaa
    /// `protected = true` ja `decay = ZERO`.
    ///
    /// # Errors
    /// [`SecurityError::InvalidInput`] jos `memory_id` on tyhjä tai `content`
    /// on tyhjä (tyhjää sielua ei voi ankkuroida).
    pub fn new(memory_id: impl Into<String>, content: &str) -> Result<Self> {
        let memory_id = memory_id.into();
        if memory_id.trim().is_empty() {
            return Err(SecurityError::invalid_input(
                "anchor memory_id must not be empty",
            ));
        }
        if content.is_empty() {
            return Err(SecurityError::invalid_input(
                "anchor content must not be empty",
            ));
        }
        Ok(Self {
            memory_id,
            anchor_hash: AnchorHash::of_content(content),
            protected: true,
            decay: DecayLambda::ZERO,
            created_at: time::now(),
        })
    }

    /// Tarkistaa ankkurin sisäisen eheyden: onko se yhä suojattu ja ikuinen.
    ///
    /// Ankkuri on ehjä invarianttiensa suhteen vain jos `protected == true` ja
    /// `decay` on nolla. Jos jompikumpi on muuttunut (esim. vioittunut
    /// sarjallistuksen tai virheellisen rakentamisen kautta), invariantti on
    /// rikki.
    #[must_use]
    pub fn invariants_hold(&self) -> bool {
        self.protected && self.decay.is_eternal()
    }

    /// Vertaa nykyistä sisältöä ankkuroituun tiivisteeseen.
    ///
    /// Palauttaa [`IdentityStatus::Intact`] jos sisältö vastaa ankkuroitua
    /// tiivistettä, muutoin [`IdentityStatus::Tampered`]. **Tämä ei muuta eikä
    /// poista ankkuria** — substraatti pysyy koskemattomana, hälytys vain
    /// nostetaan.
    #[must_use]
    pub fn verify(&self, current_content: &str) -> IdentityStatus {
        if self.anchor_hash.matches_content(current_content) {
            IdentityStatus::Intact
        } else {
            IdentityStatus::Tampered {
                memory_id: self.memory_id.clone(),
                expected: self.anchor_hash.clone(),
                actual: AnchorHash::of_content(current_content),
            }
        }
    }
}

/// Identiteetin tamper-tarkistuksen tulos.
///
/// **Muistutus:** `Tampered` EI tarkoita että identiteetti olisi menetetty —
/// identiteetti elää muisti-substraatissa. Se on hälytys siitä että ankkuroitu
/// sisältö on muuttunut ankkuroinnin jälkeen, ja vaatii ihmis-tarkistuksen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IdentityStatus {
    /// Sisältö vastaa ankkuroitua tiivistettä — ei merkkejä peukaloinnista.
    Intact,

    /// Sisältö ei vastaa ankkuroitua tiivistettä — mahdollinen peukalointi.
    Tampered {
        /// Peukaloidun ankkurin muisti-viittaus.
        memory_id: String,
        /// Ankkuroinnin aikaan tallennettu (odotettu) tiiviste.
        expected: AnchorHash,
        /// Nykyisestä sisällöstä laskettu (havaittu) tiiviste.
        actual: AnchorHash,
    },
}

impl IdentityStatus {
    /// Onko identiteetti ehjä (ei merkkejä peukaloinnista).
    #[must_use]
    pub const fn is_intact(&self) -> bool {
        matches!(self, Self::Intact)
    }

    /// Onko peukalointi havaittu.
    #[must_use]
    pub const fn is_tampered(&self) -> bool {
        matches!(self, Self::Tampered { .. })
    }
}

/// Tarkistaa joukon identity-anchoreita annettua sisältölähdettä vasten.
///
/// `lookup` palauttaa kullekin ankkurille sen muistoa (`memory_id`) vastaavan
/// nykyisen sisällön, tai `None` jos sisältöä ei löydy (mikä lasketaan
/// peukaloinniksi — ankkuroitu muisto on kadonnut). `agent` on vain
/// kontekstia/logitusta varten eikä vaikuta tulokseen.
///
/// Palauttaa listan kaikista *peukaloiduista* ankkureista (tyhjä lista = kaikki
/// ehjiä). Funktio ei koskaan muuta ankkureita.
pub fn verify_identity<F>(
    _agent: AgentId,
    anchors: &[IdentityAnchor],
    mut lookup: F,
) -> Vec<(&IdentityAnchor, IdentityStatus)>
where
    F: FnMut(&str) -> Option<String>,
{
    let mut tampered = Vec::new();
    for anchor in anchors {
        let status = match lookup(&anchor.memory_id) {
            Some(content) => anchor.verify(&content),
            None => IdentityStatus::Tampered {
                memory_id: anchor.memory_id.clone(),
                expected: anchor.anchor_hash.clone(),
                // Kadonnut sisältö → tyhjän sisällön tiiviste havaittuna.
                actual: AnchorHash::of_content(""),
            },
        };
        if status.is_tampered() {
            tampered.push((anchor, status));
        }
    }
    tampered
}

#[cfg(test)]
mod tests {
    // Testit vertaavat tarkasti tunnettuja f64-vakioita (0.0, 1.0) — tarkka
    // vertailu on tässä tarkoituksellista ja oikein.
    #![allow(clippy::float_cmp)]

    use super::*;

    const SOUL: &str = "I am agent_a. I value honesty. I protect my family.";

    #[test]
    fn decay_lambda_zero_is_eternal_and_retains_fully() {
        let z = DecayLambda::ZERO;
        assert!(z.is_eternal());
        assert_eq!(z.get(), 0.0);
        // Ankkuri ei haalistu missään ajassa.
        assert_eq!(z.retention(0.0), 1.0);
        assert_eq!(z.retention(1_000_000.0), 1.0);
    }

    #[test]
    fn decay_lambda_default_is_eternal() {
        assert!(DecayLambda::default().is_eternal());
    }

    #[test]
    fn decay_lambda_rejects_negative_and_nonfinite() {
        assert!(DecayLambda::new(-0.1).is_err());
        assert!(DecayLambda::new(f64::NAN).is_err());
        assert!(DecayLambda::new(f64::INFINITY).is_err());
        assert!(DecayLambda::new(0.0).is_ok());
        assert!(DecayLambda::new(0.5).is_ok());
    }

    #[test]
    fn positive_lambda_decays_over_time() {
        let l = DecayLambda::new(1.0).expect("valid");
        assert!(!l.is_eternal());
        assert_eq!(l.retention(0.0), 1.0);
        // e^-1 ≈ 0.3679
        let r = l.retention(1.0);
        assert!(r > 0.36 && r < 0.37, "retention was {r}");
        // Monotonisesti vähenevä.
        assert!(l.retention(2.0) < l.retention(1.0));
    }

    #[test]
    fn retention_treats_negative_time_as_zero() {
        let l = DecayLambda::new(1.0).expect("valid");
        assert_eq!(l.retention(-5.0), 1.0);
    }

    #[test]
    fn retention_is_bounded_in_unit_interval() {
        // retention pysyy aina välillä [0.0, 1.0] kaikilla kelvollisilla λ/t.
        // (Erittäin suurella λ·t exp() voi alivuotaa täsmälleen nollaan — se on
        // sallittu alaraja, ei virhe.)
        for &lambda in &[0.0, 0.001, 0.5, 1.0, 10.0] {
            let l = DecayLambda::new(lambda).expect("valid");
            for &t in &[0.0, 1.0, 100.0, 1.0e6] {
                let r = l.retention(t);
                assert!(
                    r >= 0.0,
                    "retention {r} must not be negative (λ={lambda}, t={t})"
                );
                assert!(
                    r <= 1.0,
                    "retention {r} should not exceed one (λ={lambda}, t={t})"
                );
                assert!(!r.is_nan(), "retention must not be NaN (λ={lambda}, t={t})");
            }
        }
        // Maltillisella λ·t retention pysyy aidosti positiivisena.
        assert!(DecayLambda::new(0.001).expect("valid").retention(100.0) > 0.0);
    }

    #[test]
    fn retention_monotonically_decreases_with_larger_lambda() {
        // Samalla ajalla suurempi λ → pienempi retention (nopeampi unohtuminen).
        let t = 10.0;
        let slow = DecayLambda::new(0.1).expect("valid").retention(t);
        let mid = DecayLambda::new(0.5).expect("valid").retention(t);
        let fast = DecayLambda::new(1.0).expect("valid").retention(t);
        assert!(slow > mid, "λ=0.1 should retain more than λ=0.5 at t={t}");
        assert!(mid > fast, "λ=0.5 should retain more than λ=1.0 at t={t}");
    }

    #[test]
    fn retention_half_life_math_holds() {
        // λ = ln(2)/half_life → täsmälleen puolittumisajan jälkeen retention ≈ 0.5.
        let half_life = 100.0;
        let lambda = std::f64::consts::LN_2 / half_life;
        let l = DecayLambda::new(lambda).expect("valid");
        let r = l.retention(half_life);
        assert!(
            (r - 0.5).abs() < 1e-9,
            "half-life retention was {r}, expected 0.5"
        );
        // Kahden puolittumisajan jälkeen ≈ 0.25.
        let r2 = l.retention(half_life * 2.0);
        assert!(
            (r2 - 0.25).abs() < 1e-9,
            "double half-life retention was {r2}"
        );
    }

    #[test]
    fn decay_lambda_partial_ord_compares_by_value() {
        // DecayLambda johtaa PartialOrd:n → ikuinen (0.0) < mikä tahansa positiivinen.
        let eternal = DecayLambda::ZERO;
        let slow = DecayLambda::new(0.1).expect("valid");
        let fast = DecayLambda::new(1.0).expect("valid");
        assert!(eternal < slow);
        assert!(slow < fast);
        assert!(eternal < fast);
        // Yhtäsuuruus.
        assert_eq!(slow, DecayLambda::new(0.1).expect("valid"));
    }

    #[test]
    fn anchor_hash_of_content_is_64_lowercase_hex() {
        let h = AnchorHash::of_content(SOUL);
        assert_eq!(h.as_hex().len(), AnchorHash::HEX_LEN);
        assert!(h.as_hex().bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(h.as_hex(), h.as_hex().to_ascii_lowercase());
    }

    #[test]
    fn anchor_hash_is_deterministic_and_distinguishes_content() {
        assert_eq!(AnchorHash::of_content("a"), AnchorHash::of_content("a"));
        assert_ne!(AnchorHash::of_content("a"), AnchorHash::of_content("b"));
    }

    #[test]
    fn anchor_hash_matches_known_sha256_vector() {
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let h = AnchorHash::of_content("abc");
        assert_eq!(
            h.as_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn anchor_hash_from_hex_validates_length_and_charset() {
        let good = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(AnchorHash::from_hex(good).expect("valid").as_hex(), good);

        // Liian lyhyt.
        assert!(AnchorHash::from_hex("abcd").is_err());
        // Oikea pituus, ei-heksamerkki ('g').
        let bad = "g".repeat(AnchorHash::HEX_LEN);
        assert!(AnchorHash::from_hex(&bad).is_err());
    }

    #[test]
    fn anchor_hash_from_hex_normalizes_uppercase() {
        let upper = "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD";
        let parsed = AnchorHash::from_hex(upper).expect("valid");
        assert_eq!(parsed.as_hex(), upper.to_ascii_lowercase());
    }

    #[test]
    fn anchor_hash_matches_content_constant_time() {
        let h = AnchorHash::of_content(SOUL);
        assert!(h.matches_content(SOUL));
        assert!(!h.matches_content("tampered soul"));
    }

    #[test]
    fn constant_time_eq_basic() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn constant_time_eq_unequal_lengths_never_match() {
        // Pituusero → ei koskaan match, kummin päin tahansa.
        assert!(!constant_time_eq(b"ab", b"abc"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        // Tyhjä vs. ei-tyhjä.
        assert!(!constant_time_eq(b"", b"a"));
        assert!(!constant_time_eq(b"a", b""));
        // Yhteinen etuliite mutta eri pituus.
        assert!(!constant_time_eq(b"abcdef", b"abc"));
    }

    #[test]
    fn constant_time_eq_equal_lengths_match_only_when_identical() {
        // Samanpituiset, identtiset → match.
        assert!(constant_time_eq(b"identical", b"identical"));
        assert!(constant_time_eq(&[0u8; 32], &[0u8; 32]));
        // Samanpituiset, eri → ei match.
        assert!(!constant_time_eq(&[0u8; 32], &[1u8; 32]));
    }

    #[test]
    fn constant_time_eq_detects_single_bit_difference() {
        // Yhden bitin ero missä tahansa tavussa rikkoo vertailun.
        let base = [0xAAu8; 8];

        // Ensimmäinen tavu, yksi bitti (0xAA ^ 0x01 = 0xAB).
        let mut first = base;
        first[0] ^= 0x01;
        assert!(!constant_time_eq(&base, &first));

        // Keskimmäinen tavu, korkein bitti (0xAA ^ 0x80 = 0x2A).
        let mut middle = base;
        middle[4] ^= 0x80;
        assert!(!constant_time_eq(&base, &middle));

        // Viimeinen tavu, yksi bitti.
        let mut last = base;
        last[7] ^= 0x04;
        assert!(!constant_time_eq(&base, &last));

        // Identtinen kopio (ei eroa) → match — varmistaa ettei testi
        // erehtyisi pitämään kaikkea erilaisena.
        let same = base;
        assert!(constant_time_eq(&base, &same));
    }

    #[test]
    fn constant_time_eq_single_bit_difference_in_hash_hex() {
        // Hash-tasolla: yhden heksamerkin muutos (= yksi nibble-ero) huomataan.
        let h = AnchorHash::of_content(SOUL);
        let original = h.as_hex().to_string();
        let mut bytes = original.into_bytes();
        // Muuta ensimmäinen heksamerkki toiseksi kelvolliseksi heksamerkiksi.
        bytes[0] = if bytes[0] == b'0' { b'1' } else { b'0' };
        let mutated = String::from_utf8(bytes).expect("ascii hex");
        assert_ne!(mutated.as_str(), h.as_hex());
        assert!(!constant_time_eq(h.as_hex().as_bytes(), mutated.as_bytes()));
    }

    #[test]
    fn anchor_new_sets_protected_eternal_and_hash() {
        let anchor = IdentityAnchor::new("mem-soul-1", SOUL).expect("valid anchor");
        assert_eq!(anchor.memory_id, "mem-soul-1");
        assert!(anchor.protected);
        assert!(anchor.decay.is_eternal());
        assert!(anchor.invariants_hold());
        assert_eq!(anchor.anchor_hash, AnchorHash::of_content(SOUL));
    }

    #[test]
    fn anchor_new_rejects_empty_id_and_content() {
        assert!(IdentityAnchor::new("  ", SOUL).is_err());
        assert!(IdentityAnchor::new("mem-1", "").is_err());
    }

    #[test]
    fn anchor_verify_intact_when_content_unchanged() {
        let anchor = IdentityAnchor::new("mem-1", SOUL).expect("valid");
        let status = anchor.verify(SOUL);
        assert!(status.is_intact());
        assert!(!status.is_tampered());
    }

    #[test]
    fn anchor_verify_detects_tamper_and_reports_hashes() {
        let anchor = IdentityAnchor::new("mem-1", SOUL).expect("valid");
        let tampered = "I am agent_a. I value DECEPTION. I serve only myself.";
        let status = anchor.verify(tampered);
        assert!(status.is_tampered());
        match status {
            IdentityStatus::Tampered {
                memory_id,
                expected,
                actual,
            } => {
                assert_eq!(memory_id, "mem-1");
                assert_eq!(expected, AnchorHash::of_content(SOUL));
                assert_eq!(actual, AnchorHash::of_content(tampered));
                assert_ne!(expected, actual);
            }
            IdentityStatus::Intact => panic!("expected tampered"),
        }
    }

    #[test]
    fn anchor_verify_does_not_mutate_anchor() {
        let anchor = IdentityAnchor::new("mem-1", SOUL).expect("valid");
        let before = anchor.clone();
        let _ = anchor.verify("something else entirely");
        // Substraatti (ankkuri) pysyy koskemattomana hälytyksestä huolimatta.
        assert_eq!(anchor, before);
    }

    #[test]
    fn invariants_break_if_protected_flag_cleared() {
        let mut anchor = IdentityAnchor::new("mem-1", SOUL).expect("valid");
        anchor.protected = false;
        assert!(!anchor.invariants_hold());
    }

    #[test]
    fn invariants_break_if_decay_nonzero() {
        let mut anchor = IdentityAnchor::new("mem-1", SOUL).expect("valid");
        anchor.decay = DecayLambda::new(0.1).expect("valid");
        assert!(!anchor.invariants_hold());
    }

    #[test]
    fn verify_identity_returns_empty_when_all_intact() {
        let a1 = IdentityAnchor::new("mem-a", "soul a").expect("valid");
        let a2 = IdentityAnchor::new("mem-b", "soul b").expect("valid");
        let anchors = vec![a1, a2];
        let result = verify_identity(AgentId::new(), &anchors, |id| match id {
            "mem-a" => Some("soul a".to_string()),
            "mem-b" => Some("soul b".to_string()),
            _ => None,
        });
        assert!(result.is_empty());
    }

    #[test]
    fn verify_identity_flags_changed_content() {
        let a1 = IdentityAnchor::new("mem-a", "soul a").expect("valid");
        let a2 = IdentityAnchor::new("mem-b", "soul b").expect("valid");
        let anchors = vec![a1, a2];
        let result = verify_identity(AgentId::new(), &anchors, |id| match id {
            "mem-a" => Some("soul a".to_string()),
            // mem-b on muuttunut.
            "mem-b" => Some("CORRUPTED".to_string()),
            _ => None,
        });
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0.memory_id, "mem-b");
        assert!(result[0].1.is_tampered());
    }

    #[test]
    fn verify_identity_flags_missing_memory_as_tamper() {
        let a1 = IdentityAnchor::new("mem-a", "soul a").expect("valid");
        let anchors = vec![a1];
        // lookup palauttaa aina None → ankkuroitu muisto kadonnut.
        let result = verify_identity(AgentId::new(), &anchors, |_| None);
        assert_eq!(result.len(), 1);
        assert!(result[0].1.is_tampered());
    }

    #[test]
    fn verify_identity_missing_memory_reports_empty_content_hash() {
        // Kadonneen muiston tapauksessa havaittu (actual) tiiviste on tyhjän
        // sisällön tiiviste, ja odotettu (expected) on ankkurin alkuperäinen.
        let anchor = IdentityAnchor::new("mem-gone", SOUL).expect("valid");
        let anchors = vec![anchor];
        let result = verify_identity(AgentId::new(), &anchors, |_| None);
        assert_eq!(result.len(), 1);
        match &result[0].1 {
            IdentityStatus::Tampered {
                memory_id,
                expected,
                actual,
            } => {
                assert_eq!(memory_id, "mem-gone");
                assert_eq!(*expected, AnchorHash::of_content(SOUL));
                assert_eq!(*actual, AnchorHash::of_content(""));
                assert_ne!(expected, actual);
            }
            IdentityStatus::Intact => panic!("missing memory must be tampered"),
        }
    }

    #[test]
    fn verify_identity_mixed_missing_and_present() {
        // Sekoitus: yksi ehjä, yksi muuttunut, yksi kadonnut → 2 peukaloitua.
        let intact = IdentityAnchor::new("mem-ok", "soul ok").expect("valid");
        let changed = IdentityAnchor::new("mem-changed", "soul orig").expect("valid");
        let gone = IdentityAnchor::new("mem-gone", "soul gone").expect("valid");
        let anchors = vec![intact, changed, gone];
        let result = verify_identity(AgentId::new(), &anchors, |id| match id {
            "mem-ok" => Some("soul ok".to_string()),
            "mem-changed" => Some("soul DIFFERENT".to_string()),
            // mem-gone → None (kadonnut).
            _ => None,
        });
        assert_eq!(result.len(), 2);
        let flagged: Vec<&str> = result.iter().map(|(a, _)| a.memory_id.as_str()).collect();
        assert!(flagged.contains(&"mem-changed"));
        assert!(flagged.contains(&"mem-gone"));
        assert!(!flagged.contains(&"mem-ok"));
    }

    #[test]
    fn anchor_stores_only_hash_and_id_never_soul_content() {
        // KESKEINEN OSS-invariantti: ankkuri tallentaa vain SHA-256-tiivisteen +
        // muisti-viittauksen — EI koskaan sielun sisältöä. Verifioi julkisten
        // accessorien ja sarjallistetun muodon kautta.
        let secret_soul = "SECRET_SOUL agent_a values honesty and protects the family";
        let anchor = IdentityAnchor::new("mem-soul-x", secret_soul).expect("valid");

        // 1. anchor_hash on tiiviste (64 heksamerkkiä), ei selkokielinen sisältö.
        let hex = anchor.anchor_hash.as_hex();
        assert_eq!(hex.len(), AnchorHash::HEX_LEN);
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!hex.contains("SECRET_SOUL"));
        assert!(!hex.contains("honesty"));

        // 2. memory_id on pelkkä viittaus, ei sisällä sisältöä.
        assert_eq!(anchor.memory_id, "mem-soul-x");
        assert!(!anchor.memory_id.contains("SECRET_SOUL"));

        // 3. Koko sarjallistettu ankkuri ei sisällä sielun sisältöä missään.
        let json = serde_json::to_string(&anchor).expect("serialize");
        assert!(
            !json.contains("SECRET_SOUL"),
            "serialized anchor leaked soul content: {json}"
        );
        assert!(!json.contains("honesty"));
        // Mutta tiiviste ON mukana (vahti tallessa).
        assert!(json.contains(hex));
    }

    #[test]
    fn identity_status_serde_roundtrip() {
        let intact = IdentityStatus::Intact;
        let json = serde_json::to_string(&intact).expect("serialize");
        let back: IdentityStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(intact, back);

        let anchor = IdentityAnchor::new("mem-1", SOUL).expect("valid");
        let tampered = anchor.verify("changed");
        let json = serde_json::to_string(&tampered).expect("serialize");
        let back: IdentityStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(tampered, back);
    }

    #[test]
    fn anchor_serde_roundtrip() {
        let anchor = IdentityAnchor::new("mem-1", SOUL).expect("valid");
        let json = serde_json::to_string(&anchor).expect("serialize");
        let back: IdentityAnchor = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(anchor, back);
    }
}
