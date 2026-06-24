//! Todistepaketti (proof bundle): suorituksen todennettava jälki, jossa
//! salaisuudelta näyttävät arvot on redaktoitu (KERROS A).
//!
//! Tämä moduuli toteuttaa:
//! - [`redact_value`] — rekursiivinen redaktoija, joka korvaa salaisuudelta
//!   näyttävät merkkijonot merkinnällä `[REDACTED]`,
//! - [`RedactionReport`] — yhteenveto siitä, montako arvoa redaktoitiin ja
//!   millä **kuvioiden nimillä** (ei arvoilla),
//! - [`VerificationResult`] — jälkiehtotarkistuksen tulos,
//! - [`ProofBundle`] — koottu todistepaketti, jossa syöte tiivistetään
//!   ([`sha2::Sha256`]) eikä koskaan tallenneta raakana,
//! - [`build_proof`] — apuri joka koostaa todistepaketin pyynnöstä,
//!   tuloksesta, audit-tunnisteista ja verifioinnista, ajaen redaktoinnin.
//!
//! ## OSS-raja
//! Todistepaketti ei koskaan sisällä raakaa tokenia, API-avainta tai muuta
//! salaisuutta: syöte tallennetaan vain SHA-256-tiivisteenä, ja sekä syöte-
//! että tulostekentät redaktoidaan ennen pakettiin liittämistä.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use familyclaw_core::time::Timestamp;

use crate::executor::{ActionRequest, ActionResult, ActionStatus};
use crate::ids::{ActionId, ActionTaskId, AuditEventId, ProofBundleId, SkillId};

/// Moduulin valmiusaste — säilytetään, jotta [`crate::all_modules_scaffolded`]
/// kääntyy edelleen muiden moduulien rinnalla.
pub(crate) const SCAFFOLDED: bool = true;

/// Korvausmerkki redaktoidulle arvolle.
const REDACTED: &str = "[REDACTED]";

/// Avainnimien (case-insensitive) joukko, joiden **arvo** redaktoidaan aina.
const SECRET_KEY_NAMES: &[&str] = &[
    "api_key",
    "apikey",
    "secret",
    "password",
    "token",
    "authorization",
];

/// Yhteenveto suoritetusta redaktoinnista.
///
/// Sisältää redaktoitujen arvojen lukumäärän ja löydettyjen kuvioiden
/// **nimet** (esim. `"sk-key"`, `"bearer"`, `"secret-key-name"`) — **ei
/// koskaan arvoja**. Näin raportti itse on turvallinen tallentaa.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionReport {
    /// Montako arvoa redaktoitiin.
    pub redacted_count: usize,
    /// Löydettyjen kuvioiden nimet (ei arvoja), aakkostettu ja uniikki.
    pub patterns_found: Vec<String>,
}

impl RedactionReport {
    /// Redaktoitiinko vähintään yksi arvo.
    #[must_use]
    pub fn any_redacted(&self) -> bool {
        self.redacted_count > 0
    }
}

/// Tunnistaa salaisuudelta näyttävän merkkijonon ja palauttaa osuneen kuvion
/// **nimen** (ei arvoa). `None` jos arvo ei näytä salaisuudelta.
///
/// Tunnistetut kuviot:
/// - `sk-[A-Za-z0-9]{8,}` (OpenAI-tyylinen avain) → `"sk-key"`,
/// - `AKIA[0-9A-Z]{12,}` (AWS-tyylinen access key) → `"aws-access-key"`,
/// - `Bearer <token>` → `"bearer"`,
/// - pitkä heksa (≥32 heksamerkkiä) → `"long-hex"`,
/// - base64-tyylinen ajo (≥24 merkkiä, sisältää `+`/`/`/`=`) → `"base64-blob"`.
fn match_secret_pattern(value: &str) -> Option<&'static str> {
    let trimmed = value.trim();

    // Bearer-token: "Bearer " + ainakin yksi merkki.
    if let Some(rest) = trimmed.strip_prefix("Bearer ") {
        if !rest.trim().is_empty() {
            return Some("bearer");
        }
    }

    // sk-XXXXXXXX (≥8 aakkosnumeerista skn jälkeen).
    if let Some(rest) = trimmed.strip_prefix("sk-") {
        let run = rest.chars().take_while(char::is_ascii_alphanumeric).count();
        if run >= 8 {
            return Some("sk-key");
        }
    }

    // AKIA + 12+ isoa kirjainta/numeroa.
    if let Some(rest) = trimmed.strip_prefix("AKIA") {
        let run = rest
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            .count();
        if run >= 12 {
            return Some("aws-access-key");
        }
    }

    // Pitkä heksa: ≥32 merkkiä, kaikki heksaa.
    if trimmed.len() >= 32 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some("long-hex");
    }

    // Base64-tyylinen: ≥24 merkkiä, sallitut merkit ja vähintään yksi
    // base64-erikoismerkki (+ / =), jotta tavalliset sanat eivät osu.
    if trimmed.len() >= 24
        && trimmed.contains(['+', '/', '='])
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
    {
        return Some("base64-blob");
    }

    None
}

/// Onko avainnimi (case-insensitive) tunnettu salaisuusavain.
fn is_secret_key_name(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SECRET_KEY_NAMES.iter().any(|name| *name == lower)
}

/// Redaktoi salaisuudelta näyttävät osajonot vapaamuotoisesta tekstistä.
///
/// Toisin kuin [`match_secret_pattern`] (joka tutkii koko merkkijonon),
/// tämä pilkkoo tekstin tyhjämerkeillä ja redaktoi yksittäiset *sanat*,
/// jotka näyttävät salaisuudelta. Näin esim. ylävirran virheselite
/// `"auth rejected: sk-livelivelive"` ei vuoda raakaa tokenia todisteeseen.
///
/// Lisäksi tunnistetaan kaksisanainen `Bearer <token>` -muoto, jolloin
/// koko `Bearer …` korvataan, sekä `avain=arvo`- ja `avain: arvo` -muodot,
/// joissa avaimen nimi on tunnettu salaisuusavain.
///
/// Palauttaa redaktoidun tekstin ja kasvattaa annettua raporttia jokaisesta
/// osumasta (raporttiin kirjataan vain kuvion **nimi**, ei arvoa).
fn redact_text(text: &str, report: &mut RedactionReport) -> String {
    // 1. Substring-pass: redaktoi `Bearer <token>` missä tahansa tekstissä
    //    (myös upotettuna esim. muotoon `header=Bearer xyz`).
    let bearer_redacted = redact_bearer_substrings(text, report);

    // 2. Sanapass: pilko säilyttäen tyhjämerkit ja redaktoi yksittäiset
    //    sanat (arvopohjaisesti) sekä `avain=arvo`-muodot.
    let mut out = String::with_capacity(bearer_redacted.len());
    for chunk in bearer_redacted.split_inclusive(char::is_whitespace) {
        // Erota sana ja sitä seuraava tyhjämerkki (jos on).
        let trimmed_end = chunk.trim_end_matches(char::is_whitespace);
        let trailing = &chunk[trimmed_end.len()..];

        // "key=value" / "key:value" -muoto, jossa avain on salaisuusavain.
        if let Some(redacted_kv) = redact_keyed_token(trimmed_end, report) {
            out.push_str(&redacted_kv);
            out.push_str(trailing);
            continue;
        }

        // Arvopohjainen tunnistus yksittäiselle sanalle.
        match match_secret_pattern(trimmed_end) {
            Some(pattern) => {
                report.redacted_count += 1;
                report.patterns_found.push(pattern.to_string());
                out.push_str(REDACTED);
            }
            None => out.push_str(trimmed_end),
        }
        out.push_str(trailing);
    }

    out
}

/// Redaktoi kaikki `Bearer <token>` -esiintymät tekstistä — myös upotetut,
/// esim. `Authorization: Bearer abc` tai `header=Bearer abc`. Korvaa
/// `Bearer`-sanan jälkeisen tyhjämerkein erotetun tokenin merkinnällä.
fn redact_bearer_substrings(text: &str, report: &mut RedactionReport) -> String {
    const MARKER: &str = "Bearer ";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(pos) = rest.find(MARKER) {
        let after = &rest[pos + MARKER.len()..];
        // Token = "Bearer "-merkin jälkeiset ei-tyhjämerkit.
        let token_len = after
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map_or(after.len(), |(i, _)| i);
        if token_len == 0 {
            // "Bearer " ilman tokenia — kopioi sellaisenaan ja jatka.
            out.push_str(&rest[..pos + MARKER.len()]);
            rest = after;
            continue;
        }
        report.redacted_count += 1;
        report.patterns_found.push("bearer".to_string());
        out.push_str(&rest[..pos + MARKER.len()]);
        out.push_str(REDACTED);
        rest = &after[token_len..];
    }
    out.push_str(rest);
    out
}

/// Redaktoi `avain=arvo` / `avain: arvo` -muotoisen sanan arvon, jos avaimen
/// nimi (ennen erotinta) on tunnettu salaisuusavain. Palauttaa `None`, jos sana
/// ei ole tällainen muoto.
fn redact_keyed_token(word: &str, report: &mut RedactionReport) -> Option<String> {
    let sep = word.find(['=', ':'])?;
    let key = word[..sep].trim();
    if key.is_empty() || !is_secret_key_name(key) {
        return None;
    }
    let value = &word[sep + 1..];
    if value.trim().is_empty() {
        return None;
    }
    report.redacted_count += 1;
    report
        .patterns_found
        .push(format!("secret-key:{}", key.to_ascii_lowercase()));
    let separator = &word[sep..=sep];
    Some(format!("{key}{separator}{REDACTED}"))
}

/// Redaktoi salaisuudelta näyttävät arvot rekursiivisesti annetusta
/// [`serde_json::Value`]-rakenteesta.
///
/// Palauttaa redaktoidun kopion sekä [`RedactionReport`]-yhteenvedon. Korvataan
/// merkinnällä `[REDACTED]` jos:
/// - merkkijonon arvo itse näyttää salaisuudelta (kuviotunnistus), tai
/// - arvo on objektin kentässä, jonka **nimi** on tunnettu salaisuusavain
///   (`api_key`, `apikey`, `secret`, `password`, `token`, `authorization`) —
///   riippumatta arvon muodosta.
///
/// Alkuperäistä syötettä ei muteta. Tuloraporttiin ei koskaan päädy raakoja
/// salaisia arvoja, vain kuvioiden nimet.
#[must_use]
pub fn redact_value(value: &Value) -> (Value, RedactionReport) {
    let mut report = RedactionReport::default();
    let redacted = redact_inner(value, None, &mut report);
    report.patterns_found.sort_unstable();
    report.patterns_found.dedup();
    (redacted, report)
}

/// Sisäinen rekursio: `parent_key` on objektin kentän nimi jossa `value`
/// sijaitsee (jos sellainen on), jotta avainnimipohjainen redaktointi toimii.
fn redact_inner(value: &Value, parent_key: Option<&str>, report: &mut RedactionReport) -> Value {
    match value {
        Value::String(s) => {
            // Avainnimipohjainen redaktointi: kentän nimi paljastaa salaisuuden.
            if let Some(key) = parent_key {
                if is_secret_key_name(key) && !s.is_empty() {
                    report.redacted_count += 1;
                    report
                        .patterns_found
                        .push(format!("secret-key:{}", key.to_ascii_lowercase()));
                    return Value::String(REDACTED.to_string());
                }
            }
            // Arvopohjainen redaktointi: merkkijono näyttää salaisuudelta.
            if let Some(pattern) = match_secret_pattern(s) {
                report.redacted_count += 1;
                report.patterns_found.push(pattern.to_string());
                return Value::String(REDACTED.to_string());
            }
            Value::String(s.clone())
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_inner(item, None, report))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), redact_inner(v, Some(k), report));
            }
            Value::Object(out)
        }
        // Luvut, totuusarvot ja null eivät voi olla salaisuuksia.
        other => other.clone(),
    }
}

/// Redaktoi salaisuudelta näyttävät arvot **syvällä** rekursiivisesti annetusta
/// [`serde_json::Value`]-rakenteesta — myös vapaamuotoiseen tekstiin **upotetut**
/// salaisuudet.
///
/// Ero [`redact_value`]:hin: siinä missä [`redact_value`] redaktoi merkkijonon
/// vain jos (a) sen kentän nimi on tunnettu salaisuusavain tai (b) **koko**
/// merkkijono näyttää salaisuudelta, tämä variantti ajaa lisäksi
/// `redact_text`-osajono­pass:in jokaiselle merkkijonolehdelle, joka ei jo
/// mennyt kokonaan redaktoiduksi. Näin esim. vapaamuotoinen työkaluargumentti
/// `{"prompt":"deploy using sk-livelivelivelive then ..."}` ei vuoda raakaa
/// tokenia levylle, vaikka kentän nimi (`prompt`) ei ole salaisuusavain eikä
/// koko arvo ole pelkkä token.
///
/// Käytetään jatkettavan vuoron ([`crate::ApprovalId`]-avaimella tallennettava
/// resumable-turn) **levylle persistoitavan** viestipinon työkaluargumenteille,
/// joissa salaisuus voi piillä mallin tuottaman vapaatekstin sisällä.
///
/// Palauttaa redaktoidun kopion sekä [`RedactionReport`]-yhteenvedon. Alkuperäistä
/// syötettä ei muteta, eikä raportti koskaan kanna raakoja salaisia arvoja.
#[must_use]
pub fn redact_value_deep(value: &Value) -> (Value, RedactionReport) {
    let mut report = RedactionReport::default();
    let redacted = redact_inner_deep(value, None, &mut report);
    report.patterns_found.sort_unstable();
    report.patterns_found.dedup();
    (redacted, report)
}

/// Kuten [`redact_inner`], mutta merkkijonolehdille ajetaan lisäksi
/// [`redact_text`]-osajono­pass, jotta vapaamuotoiseen tekstiin upotetut
/// salaisuudet (esim. `"deploy using sk-live..."`) eivät jää redaktoimatta.
fn redact_inner_deep(
    value: &Value,
    parent_key: Option<&str>,
    report: &mut RedactionReport,
) -> Value {
    match value {
        Value::String(s) => {
            // 1. Avainnimipohjainen redaktointi: kentän nimi paljastaa salaisuuden.
            if let Some(key) = parent_key {
                if is_secret_key_name(key) && !s.is_empty() {
                    report.redacted_count += 1;
                    report
                        .patterns_found
                        .push(format!("secret-key:{}", key.to_ascii_lowercase()));
                    return Value::String(REDACTED.to_string());
                }
            }
            // 2. Arvopohjainen redaktointi: koko merkkijono näyttää salaisuudelta.
            if let Some(pattern) = match_secret_pattern(s) {
                report.redacted_count += 1;
                report.patterns_found.push(pattern.to_string());
                return Value::String(REDACTED.to_string());
            }
            // 3. Osajono­pass: salaisuus UPOTETTUNA vapaamuotoiseen tekstiin.
            //    Pilkkoo tyhjämerkeillä ja redaktoi yksittäiset salaisuussanat +
            //    `Bearer …`/`avain=arvo`-muodot. Jos mikään ei osu, teksti palautuu
            //    sellaisenaan (ei turhaa kopiota merkitykseltään).
            Value::String(redact_text(s, report))
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_inner_deep(item, None, report))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), redact_inner_deep(v, Some(k), report));
            }
            Value::Object(out)
        }
        // Luvut, totuusarvot ja null eivät voi olla salaisuuksia.
        other => other.clone(),
    }
}

/// Redaktoi salaisuudelta näyttävät osajonot vapaamuotoisesta **tekstistä**
/// (ei JSON-rakenteesta).
///
/// Tämä on julkinen kääre `redact_text`-osajono­passille, jotta `familyclaw-agent`
/// voi redaktoida jatkettavan vuoron viestipinon **tekstisisällön** (system-/user-/
/// tool-viestien `content`) ennen levylle persistointia. Pilkkoo tekstin
/// tyhjämerkeillä ja redaktoi yksittäiset salaisuussanat sekä `Bearer <token>`-
/// ja `avain=arvo`-muodot, joissa avain on tunnettu salaisuusavain.
///
/// Palauttaa redaktoidun tekstin sekä [`RedactionReport`]-yhteenvedon. Raportti
/// kantaa vain kuvioiden **nimet**, ei koskaan raakoja arvoja.
#[must_use]
pub fn redact_free_text(text: &str) -> (String, RedactionReport) {
    let mut report = RedactionReport::default();
    let redacted = redact_text(text, &mut report);
    report.patterns_found.sort_unstable();
    report.patterns_found.dedup();
    (redacted, report)
}

/// Laskee syötteen SHA-256-tiivisteen heksamerkkijonona.
///
/// Syöte sarjallistetaan ensin kanoniseen JSON-muotoon. Tiiviste tallennetaan
/// todisteeseen raakapayloadin sijasta, jottei salaisuus koskaan päädy levylle.
///
/// # Errors
/// Palauttaa [`crate::ActionError::Proof`] jos syötteen sarjallistus epäonnistuu.
pub fn sha256_hex(value: &Value) -> crate::Result<String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| crate::ActionError::Proof(format!("input serialize failed: {e}")))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Jälkiehtotarkistuksen (verify-vaihe) tulos.
///
/// Kuvaa tarkistettiinko tulos ja mitkä tarkistukset ajettiin. `notes` on
/// vapaamuotoinen ihmisluettava selite (EI salaisuuksia).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationResult {
    /// Läpäisikö tulos verifioinnin.
    pub verified: bool,
    /// Ajettujen tarkistusten nimet/kuvaukset.
    pub checks: Vec<String>,
    /// Vapaamuotoinen selite (EI salaisuuksia).
    pub notes: String,
}

impl VerificationResult {
    /// Onnistunut verifiointi annetuilla tarkistuksilla.
    #[must_use]
    pub fn passed(checks: Vec<String>, notes: impl Into<String>) -> Self {
        Self {
            verified: true,
            checks,
            notes: notes.into(),
        }
    }

    /// Epäonnistunut verifiointi annetuilla tarkistuksilla.
    #[must_use]
    pub fn failed(checks: Vec<String>, notes: impl Into<String>) -> Self {
        Self {
            verified: false,
            checks,
            notes: notes.into(),
        }
    }
}

/// Koottu todistepaketti yhdestä suoritetusta toiminnosta.
///
/// Sisältää tiivistetyn syötteen, redaktoidun tulosteen, suoritusajat,
/// viittaukset audit-tapahtumiin sekä verifiointi- ja redaktointiyhteenvedot.
/// Paketti on suunniteltu tallennettavaksi sellaisenaan: se ei koskaan sisällä
/// raakaa salaisuutta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofBundle {
    /// Todistepaketin yksilöivä tunniste.
    pub id: ProofBundleId,
    /// Tehtävä jonka osana toiminto suoritettiin.
    pub task_id: ActionTaskId,
    /// Suoritetun taidon tunniste.
    pub skill_id: SkillId,
    /// Suoritetun toiminnon tunniste.
    pub action_id: ActionId,
    /// Toiminnon lopputila.
    pub status: ActionStatus,
    /// Suorituksen alkuhetki (injektoitu).
    pub started_at: Timestamp,
    /// Suorituksen päättymishetki (injektoitu).
    pub finished_at: Timestamp,
    /// Syötteen SHA-256-tiiviste (heksa) — EI raakaa payloadia.
    pub input_hash: String,
    /// Lyhyt ihmisluettava yhteenveto tuloksesta.
    pub output_summary: String,
    /// Redaktoitu tuloste (raaka tuloste salaisuudet poistettuna).
    pub redacted_output: Value,
    /// Onko tuloste peräisin epäluotettavasta lähteestä (taint).
    pub untrusted: bool,
    /// Tähän toimintoon liittyvien audit-tapahtumien tunnisteet.
    pub audit_event_ids: Vec<AuditEventId>,
    /// Verifiointivaiheen tulos.
    pub verification: VerificationResult,
    /// Redaktointiyhteenveto (syöte + tuloste yhdistettynä).
    pub redaction: RedactionReport,
}

/// Koostaa [`ProofBundle`]:n pyynnöstä, tuloksesta, audit-tunnisteista ja
/// verifioinnista.
///
/// Vaiheet:
/// 1. laskee syötteen SHA-256-tiivisteen (raakapayloadia ei tallenneta),
/// 2. redaktoi sekä syötteen että tulosteen ([`redact_value`]),
/// 3. yhdistää redaktointiraportit yhdeksi,
/// 4. säilyttää tulosteen `untrusted`-leiman sellaisenaan tuloksesta — luotettu
///    lähde voi nollata sen jo [`ActionResult`]-tasolla.
///
/// Syötettä redaktoidaan vain raportointia varten; pakettiin tallennetaan vain
/// tiiviste, ei (edes redaktoitua) syötettä, jotta payload ei vuoda muodossakaan.
///
/// # Errors
/// Palauttaa [`crate::ActionError::Proof`] jos syötteen tiivistys epäonnistuu.
pub fn build_proof(
    request: &ActionRequest,
    result: &ActionResult,
    audit_event_ids: Vec<AuditEventId>,
    verification: VerificationResult,
) -> crate::Result<ProofBundle> {
    let input_hash = sha256_hex(&request.payload)?;

    // Redaktoi syöte (vain raporttia varten) ja tuloste (talletettavaksi).
    let (_redacted_input, input_report) = redact_value(&request.payload);
    let (redacted_output, output_report) = redact_value(&result.raw_output_redacted);

    // Yhdistä redaktointiraportit.
    let mut combined = RedactionReport {
        redacted_count: input_report.redacted_count + output_report.redacted_count,
        patterns_found: input_report.patterns_found,
    };
    combined.patterns_found.extend(output_report.patterns_found);

    // Redaktoi myös vapaatekstikentät, jotka kopioidaan todisteeseen
    // sellaisenaan (output_summary, verification.notes/checks). Nämä eivät
    // kulje redact_value:n läpi koska ne ovat String-kenttiä, ei JSON-arvoja,
    // joten ylävirran virheselite voisi muuten vuotaa raakaa tokenia.
    let output_summary = redact_text(&result.output_summary, &mut combined);
    let VerificationResult {
        verified,
        checks,
        notes,
    } = verification;
    let verification = VerificationResult {
        verified,
        checks: checks
            .into_iter()
            .map(|c| redact_text(&c, &mut combined))
            .collect(),
        notes: redact_text(&notes, &mut combined),
    };

    combined.patterns_found.sort_unstable();
    combined.patterns_found.dedup();

    Ok(ProofBundle {
        id: ProofBundleId::new(),
        task_id: request.task_id,
        skill_id: request.skill_id,
        action_id: request.action_id,
        status: result.status,
        started_at: request.now,
        finished_at: result.finished_at,
        input_hash,
        output_summary,
        redacted_output,
        untrusted: result.untrusted,
        audit_event_ids,
        verification,
        redaction: combined,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::ActionExecutor;
    use familyclaw_core::time::from_unix_secs;
    use serde_json::json;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    /// Rakentaa salaisuudelta näyttävän arvon ajonaikaisella konkatenoinnilla,
    /// jottei lähdekoodissa ole >=10-merkkistä literaalia salaisuus-kentän
    /// vieressä (Layer B -audit) eikä oikeaa avainta.
    fn fake_secret() -> String {
        format!("sk-{}", "live".repeat(4))
    }

    #[test]
    fn detects_sk_key() {
        assert_eq!(match_secret_pattern(&fake_secret()), Some("sk-key"));
    }

    #[test]
    fn detects_bearer_and_aws_and_hex() {
        let bearer = format!("Bearer {}", "abcd".repeat(3));
        assert_eq!(match_secret_pattern(&bearer), Some("bearer"));

        let aws = format!("AKIA{}", "ABCD1234".repeat(2));
        assert_eq!(match_secret_pattern(&aws), Some("aws-access-key"));

        let hex = "a".repeat(40);
        assert_eq!(match_secret_pattern(&hex), Some("long-hex"));
    }

    #[test]
    fn ordinary_strings_are_not_secrets() {
        assert_eq!(match_secret_pattern("hello world"), None);
        assert_eq!(match_secret_pattern("general"), None);
        assert_eq!(match_secret_pattern("agent_a"), None);
    }

    #[test]
    fn redacts_value_by_pattern() {
        let secret = fake_secret();
        let input = json!({ "note": secret.clone(), "ok": "general" });
        let (out, report) = redact_value(&input);
        assert_eq!(out["note"], json!(REDACTED));
        assert_eq!(out["ok"], json!("general"));
        assert!(report.any_redacted());
        let serialized = serde_json::to_string(&out).expect("serialize");
        assert!(!serialized.contains(&secret));
    }

    #[test]
    fn redacts_value_by_key_name() {
        // Lyhyt, vaaraton arvo mutta salaisuusavaimen alla → silti redaktoidaan.
        let input = json!({ "api_key": "x", "user": "agent_a" });
        let (out, report) = redact_value(&input);
        assert_eq!(out["api_key"], json!(REDACTED));
        assert_eq!(out["user"], json!("agent_a"));
        assert_eq!(report.redacted_count, 1);
    }

    #[test]
    fn redacts_recursively_in_arrays_and_objects() {
        let secret = fake_secret();
        let input = json!({
            "nested": { "deep": [ { "token": secret.clone() }, "general" ] }
        });
        let (out, _report) = redact_value(&input);
        let serialized = serde_json::to_string(&out).expect("serialize");
        assert!(!serialized.contains(&secret));
        assert!(serialized.contains(REDACTED));
    }

    #[test]
    fn redact_value_misses_secret_embedded_in_free_text_but_deep_catches_it() {
        // Tämä on juuri se aukko jonka defect #2 raportoi: salaisuus piilee
        // SUUREMMAN vapaatekstin sisällä, kentän nimi EI ole salaisuusavain,
        // eikä koko arvo ole pelkkä token. `redact_value` jättää sen raakana.
        let secret = fake_secret();
        let input = json!({ "prompt": format!("deploy using {secret} then ship") });

        // Vanha (matala) redaktointi EI nappaa upotettua salaisuutta.
        let (shallow, shallow_report) = redact_value(&input);
        let shallow_json = serde_json::to_string(&shallow).expect("serialize");
        assert!(
            shallow_json.contains(&secret),
            "redact_value is documented as missing embedded secrets (regression sentinel)"
        );
        assert!(!shallow_report.any_redacted());

        // Uusi (syvä) redaktointi nappaa sen.
        let (deep, deep_report) = redact_value_deep(&input);
        let deep_json = serde_json::to_string(&deep).expect("serialize");
        assert!(
            !deep_json.contains(&secret),
            "redact_value_deep must redact secrets embedded in free-text args"
        );
        assert!(deep_json.contains(REDACTED));
        assert!(deep_report.any_redacted());
        // Ympäröivä vaaraton teksti säilyy luettavana.
        assert!(deep_json.contains("deploy using"));
        assert!(deep_json.contains("then ship"));
    }

    #[test]
    fn redact_value_deep_still_redacts_keyed_and_whole_value_secrets() {
        // Syvä variantti EI saa heikentää matalan redaktoinnin takeita:
        // avainnimi- ja koko-arvo-redaktointi toimivat edelleen.
        let secret = fake_secret();
        let input = json!({ "api_key": "x", "note": secret.clone(), "ok": "general" });
        let (out, report) = redact_value_deep(&input);
        assert_eq!(out["api_key"], json!(REDACTED), "key-name redaction intact");
        assert_eq!(out["note"], json!(REDACTED), "whole-value redaction intact");
        assert_eq!(out["ok"], json!("general"), "innocent value preserved");
        assert!(report.any_redacted());
    }

    #[test]
    fn redact_free_text_masks_embedded_secret_in_message_content() {
        // user/system-viestin sisältö voi kantaa salaisuuden vapaatekstinä.
        let secret = fake_secret();
        let content = format!("here is my key {secret} please use it");
        let (redacted, report) = redact_free_text(&content);
        assert!(
            !redacted.contains(&secret),
            "redact_free_text must mask secrets embedded in message content"
        );
        assert!(redacted.contains(REDACTED));
        assert!(report.any_redacted());
        // Vaaraton teksti säilyy.
        assert!(redacted.contains("here is my key"));
    }

    #[test]
    fn redact_free_text_leaves_innocent_text_untouched() {
        let (redacted, report) = redact_free_text("draft a github issue about login");
        assert_eq!(redacted, "draft a github issue about login");
        assert!(!report.any_redacted());
    }

    #[test]
    fn sha256_hex_is_stable_and_64_chars() {
        let v = json!({ "a": 1, "b": "x" });
        let h1 = sha256_hex(&v).expect("hash");
        let h2 = sha256_hex(&v).expect("hash");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn verification_constructors() {
        let ok = VerificationResult::passed(vec!["status".into()], "ok");
        assert!(ok.verified);
        let bad = VerificationResult::failed(vec!["status".into()], "nope");
        assert!(!bad.verified);
    }

    /// Apuri: rakentaa suorituspyynnön testikäyttöön.
    fn request(payload: Value) -> ActionRequest {
        ActionRequest::new(
            ActionId::new(),
            SkillId::new(),
            ActionTaskId::new(),
            payload,
            at(1_700_000_000),
        )
    }

    #[tokio::test]
    async fn successful_mock_action_creates_proof_bundle() {
        let exec = crate::executor::MockActionExecutor::succeeding(json!({ "delivered": true }));
        let req = request(json!({ "to": "general", "user": "agent_a" }));
        let result = exec.execute(req.clone()).await.expect("execute");

        let audit_ids = vec![AuditEventId::new(), AuditEventId::new()];
        let verification = VerificationResult::passed(vec!["status_succeeded".into()], "ok");
        let proof =
            build_proof(&req, &result, audit_ids.clone(), verification).expect("build proof");

        assert_eq!(proof.status, ActionStatus::Succeeded);
        assert_eq!(proof.task_id, req.task_id);
        assert_eq!(proof.skill_id, req.skill_id);
        assert_eq!(proof.action_id, req.action_id);
        assert_eq!(proof.audit_event_ids, audit_ids);
        assert!(!proof.id.is_nil());
        assert_eq!(proof.input_hash.len(), 64);
        assert!(proof.verification.verified);
        assert_eq!(proof.started_at, req.now);
        assert_eq!(proof.finished_at, result.finished_at);
    }

    #[tokio::test]
    async fn failed_mock_action_creates_failed_proof_bundle() {
        let exec = crate::executor::MockActionExecutor::failing("upstream timeout");
        let req = request(json!({ "to": "general" }));
        let result = exec.execute(req.clone()).await.expect("execute");

        let verification =
            VerificationResult::failed(vec!["status_failed".into()], "action did not succeed");
        let proof = build_proof(&req, &result, vec![AuditEventId::new()], verification)
            .expect("build proof");

        assert_eq!(proof.status, ActionStatus::Failed);
        assert!(!proof.verification.verified);
        assert_eq!(proof.output_summary, "upstream timeout");
    }

    #[tokio::test]
    async fn secret_looking_input_is_redacted_in_proof() {
        // Salaisuus rakennetaan ajonaikaisella konkatenoinnilla — ei literaalia lähteessä.
        let secret = fake_secret();
        let payload = json!({ "to": "general", "note": secret.clone() });

        // Suoritus kaiuttaa syötteen tulosteeseen (taintattu lähde).
        let exec = crate::executor::MockActionExecutor::succeeding(payload.clone());
        let req = request(payload);
        let result = exec.execute(req.clone()).await.expect("execute");

        let proof = build_proof(
            &req,
            &result,
            vec![],
            VerificationResult::passed(vec!["redaction".into()], "redacted"),
        )
        .expect("build proof");

        // Tuloste redaktoitu.
        let out = serde_json::to_string(&proof.redacted_output).expect("serialize output");
        assert!(out.contains(REDACTED));
        assert!(!out.contains(&secret));
        assert!(proof.redaction.any_redacted());

        // Koko todiste (sis. input_hash) ei sisällä raakaa salaisuutta.
        let whole = serde_json::to_string(&proof).expect("serialize proof");
        assert!(!whole.contains(&secret));
    }

    #[tokio::test]
    async fn untrusted_output_is_marked_untrusted() {
        let exec = crate::executor::MockActionExecutor::succeeding(json!({ "ok": true }));
        let req = request(json!({ "to": "general" }));
        let result = exec.execute(req.clone()).await.expect("execute");
        assert!(result.untrusted, "mock output is untrusted by default");

        let proof = build_proof(
            &req,
            &result,
            vec![],
            VerificationResult::passed(vec!["taint".into()], "ok"),
        )
        .expect("build proof");
        assert!(proof.untrusted);

        // Eksplisiittisesti luotettu lähde nollaa leiman.
        let trusted_exec =
            crate::executor::MockActionExecutor::succeeding(json!({ "ok": true })).trusted();
        let trusted_result = trusted_exec.execute(req.clone()).await.expect("execute");
        let trusted_proof = build_proof(
            &req,
            &trusted_result,
            vec![],
            VerificationResult::passed(vec!["taint".into()], "ok"),
        )
        .expect("build proof");
        assert!(!trusted_proof.untrusted);
    }

    #[tokio::test]
    async fn output_summary_leaking_secret_is_redacted() {
        // Hyökkäys: ylävirran virheselite vuotaa tokenin output_summaryyn,
        // joka kopioidaan todisteeseen vapaatekstinä. Tämä kenttä EI kulje
        // redact_value:n läpi (se redaktoi vain JSON-arvot, ei String-kenttiä).
        let sk = fake_secret();
        let leaky_summary = format!("upstream auth rejected: {sk}");

        let exec = crate::executor::MockActionExecutor::failing(leaky_summary);
        let req = request(json!({ "to": "general" }));
        let result = exec.execute(req.clone()).await.expect("execute");

        let proof = build_proof(
            &req,
            &result,
            vec![AuditEventId::new()],
            VerificationResult::failed(vec!["status_failed".into()], "did not succeed"),
        )
        .expect("build proof");

        let whole = serde_json::to_string(&proof).expect("serialize proof");
        assert!(
            !whole.contains(&sk),
            "proof must not contain raw secret leaked via output_summary"
        );
    }

    #[tokio::test]
    async fn verification_notes_and_checks_leaking_secret_are_redacted() {
        // Hyökkäys: verifiointivaiheen notes/checks vuotaa tokenin
        // todisteeseen vapaatekstinä — myös nämä kentät pitää redaktoida.
        let sk = fake_secret();
        let bearer = format!("Bearer {}", "abcd".repeat(3));

        let exec = crate::executor::MockActionExecutor::succeeding(json!({ "ok": true }));
        let req = request(json!({ "to": "general" }));
        let result = exec.execute(req.clone()).await.expect("execute");

        let proof = build_proof(
            &req,
            &result,
            vec![],
            VerificationResult::passed(
                vec![format!("auth_header={bearer}")],
                format!("downstream returned {sk}"),
            ),
        )
        .expect("build proof");

        let whole = serde_json::to_string(&proof).expect("serialize proof");
        assert!(
            !whole.contains(&sk),
            "proof must not contain raw secret leaked via verification.notes"
        );
        assert!(
            !whole.contains(&bearer),
            "proof must not contain raw secret leaked via verification.checks"
        );
    }

    #[tokio::test]
    async fn proof_never_contains_raw_secret_values() {
        // Useita salaisuusmuotoja eri kentissä, mukaan lukien salaisuusavain.
        let sk = fake_secret();
        let bearer = format!("Bearer {}", "abcd".repeat(3));
        let hex = "a".repeat(40);
        let payload = json!({
            "to": "general",
            "blob": sk.clone(),
            "auth": bearer.clone(),
            "digest": hex.clone(),
            "api_key": "x"
        });

        let exec = crate::executor::MockActionExecutor::succeeding(payload.clone());
        let req = request(payload);
        let result = exec.execute(req.clone()).await.expect("execute");

        let proof = build_proof(
            &req,
            &result,
            vec![AuditEventId::new()],
            VerificationResult::passed(vec!["no_secrets".into()], "clean"),
        )
        .expect("build proof");

        let whole = serde_json::to_string(&proof).expect("serialize proof");

        // Sarjallistettu todiste ei sisällä yhtäkään raakaa salaista arvoa.
        for needle in [&sk, &bearer, &hex] {
            assert!(
                !whole.contains(needle.as_str()),
                "proof must not contain raw secret: {needle}"
            );
        }
        // Mutta redaktointimerkki on läsnä.
        assert!(whole.contains(REDACTED));
    }
}
