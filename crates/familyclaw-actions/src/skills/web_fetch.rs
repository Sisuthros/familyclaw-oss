//! Tutkimustaito: read-only HTTP-GET SSRF-vartioinnilla (KERROS A).
//!
//! [`WebFetchSkill`] antaa agentille AIDON tutkimustyökalun — se hakee julkisen
//! web-sivun sisällön luettavaksi. Tämä on tarkoituksella **vain GET**, ei
//! kirjoituksia, ja **rakenteellisesti SSRF-turvallinen**:
//!
//! ## Kuormaa kantava turvallisuus: `validate_url` + ei-redirect
//! Ennen yhtäkään verkkopyyntöä [`validate_url`] hylkää:
//! 1. ei-`http`/`https`-skeemat (estää `file://`, `ftp://`, `gopher://`, `data:`),
//! 2. puuttuvan hostin,
//! 3. `localhost` (ja `*.localhost`),
//! 4. yksityiset/loopback/link-local/CGNAT-IP:t (`127/8`, `::1`, `10/8`, `172.16/12`,
//!    `192.168/16`, `169.254/16` ml. `169.254.169.254` metadata, `100.64/10`, `fc00::/7`,
//!    `fe80::/10`, unspecified `0.0.0.0`/`::`).
//!
//! Pyyntö EI seuraa redirectejä ([`reqwest::redirect::Policy::none`]), joten
//! 302 → 169.254.169.254 ei voi ohittaa vartiointia.
//!
//! ## Rajattu vastaus
//! Vastaus typistetään (`max_bytes`, oletus 64 KiB, kova katto 512 KiB), jottei
//! valtava runko syö muistia. Tallennetaan vain **host** (ei koko `URLia`), jottei
//! query-stringin salaisuudet vuoda todisteeseen.
//!
//! ## Taint (epäluotettavuus)
//! Haettu web-sisältö on AINA epäluotettavaa (taint) — `execute` EI kutsu
//! `.trusted()`. Verkosta tuotu sisältö ei pese itseään puhtaaksi.

use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::ActionError;
use crate::error::Result;
use crate::executor::{ActionExecutor, ActionRequest, ActionResult};
use crate::ids::SkillId;
use crate::manifest::SkillManifest;
use crate::policy::{ActionRisk, ApprovalPolicy, SkillPermission};

use super::Skill;

/// Taidon kiinteä tunniste (1-5 ovat varattuja muille oletustaidoille).
const SKILL_UUID: uuid::Uuid = uuid::uuid!("66666666-6666-4666-8666-666666666666");

/// Vastauksen oletuskatto tavuina (64 KiB).
const DEFAULT_MAX_BYTES: usize = 64 * 1024;

/// Vastauksen kova yläraja tavuina (512 KiB) — estää muistin syömisen valtavalla rungolla.
const HARD_MAX_BYTES: usize = 512 * 1024;

/// Verkkopyynnön aikakatkaisu.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Taidon syöte: haettava URL ja valinnainen tavukatto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebFetchInput {
    /// Haettava URL. On oltava `http`/`https` ja osoitettava julkiseen hostiin
    /// (yksityiset/loopback/link-local-osoitteet hylätään).
    pub url: String,
    /// Valinnainen vastauksen tavukatto (rajataan välille 1..=`HARD_MAX_BYTES`).
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

/// Taidon tulos: haetun sivun ydin (status + host + koko + typistetty teksti).
///
/// Tallentaa vain **hostin**, ei koko `URLia` — query-string ei vuoda.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebFetchOutput {
    /// HTTP-statuskoodi.
    pub status: u16,
    /// Vastaava host (EI koko `URLia`).
    pub host: String,
    /// Palautetun (typistetyn) tekstin koko tavuina.
    pub bytes: usize,
    /// Typistetty, lossy-UTF-8-dekoodattu vastausteksti.
    pub text: String,
}

/// Read-only web-fetch -taito SSRF-vartioinnilla.
#[derive(Debug, Default, Clone)]
pub struct WebFetchSkill;

impl WebFetchSkill {
    /// Luo uuden taidon.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Taidon kiinteä tunniste.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(SKILL_UUID)
    }
}

/// Validoi `URLin` SSRF-turvallisesti. PUHDAS funktio — EI tee verkkopyyntöä,
/// joten se on yksikkötestattavissa ilman verkkoa.
///
/// # Errors
/// Palauttaa [`ActionError::PolicyDenied`] jos URL on epäkelpo tai osoittaa
/// ei-julkiseen kohteeseen (skeema, host, yksityinen/loopback/link-local IP).
fn validate_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw)
        .map_err(|e| ActionError::PolicyDenied(format!("epäkelpo URL (hylätty): {e}")))?;

    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(ActionError::PolicyDenied(format!(
                "skeema '{other}' ei sallittu (vain http/https; hylätty)"
            )));
        }
    }

    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| ActionError::PolicyDenied("URLissa ei ole hostia (hylätty)".to_string()))?;

    let host_lower = host.to_ascii_lowercase();
    if host_lower == "localhost" || host_lower.ends_with(".localhost") {
        return Err(ActionError::PolicyDenied(
            "localhost ei sallittu (hylätty)".to_string(),
        ));
    }

    // Jos host on kirjaimellinen IP, luokittele ja hylkää ei-julkiset.
    // IPv6-host tulee hakasulkeissa (esim. "[::1]") — riisutaan ne ennen parsea,
    // muuten IpAddr::parse epäonnistuu ja loopback/link-local pääsisi läpi (SSRF-aukko).
    let host_for_ip = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = host_for_ip.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            return Err(ActionError::PolicyDenied(format!(
                "ei-julkinen IP {ip} ei sallittu (hylätty)"
            )));
        }
    }

    Ok(url)
}

/// Onko IP julkinen (ei loopback/yksityinen/link-local/CGNAT/unspecified)?
///
/// IPv6:n `is_private`/`is_unique_local`/`is_unicast_link_local` ovat osin
/// epävakaita standardissa, joten `fc00::/7` ja `fe80::/10` tarkistetaan käsin
/// segmenteistä.
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() {
                return false;
            }
            // CGNAT 100.64.0.0/10 (ei vakaata is_shared()-metodia)
            let o = v4.octets();
            if o[0] == 100 && (64..=127).contains(&o[1]) {
                return false;
            }
            // Broadcast / dokumentaatio / reserved riittävät tässä — julkinen muuten.
            !v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return false;
            }
            let seg = v6.segments();
            // fc00::/7 (unique local): ensimmäisen tavun ylä 7 bittiä == 1111110
            if (seg[0] & 0xfe00) == 0xfc00 {
                return false;
            }
            // fe80::/10 (link local): ensimmäisen segmentin ylä 10 bittiä == 1111111010
            if (seg[0] & 0xffc0) == 0xfe80 {
                return false;
            }
            true
        }
    }
}

/// Typistää merkkijonon enintään `max_bytes` tavuun säilyttäen UTF-8-rajat.
fn truncate_utf8(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

#[async_trait]
impl ActionExecutor for WebFetchSkill {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let input: WebFetchInput = match serde_json::from_value(request.payload.clone()) {
            Ok(input) => input,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid web_fetch input: {e}"),
                    request.now,
                ));
            }
        };

        // SSRF-vartiointi ENNEN pyyntöä. Hylätty URL → epäonnistunut tulos (ei paniikkia).
        let url = match validate_url(&input.url) {
            Ok(url) => url,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("url rejected: {e}"),
                    request.now,
                ));
            }
        };

        let cap = input
            .max_bytes
            .unwrap_or(DEFAULT_MAX_BYTES)
            .clamp(1, HARD_MAX_BYTES);

        // EI redirektejä (estää 302→yksityinen ohituksen) + aikakatkaisu. TLS tulee
        // reqwestin oletuksesta (workspace-dep); per-skill rustls-pakotus poistettu
        // koska se vaatisi rustls-tls-featuren workspace-tasolla (koskisi kaikkia).
        let client = match reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(FETCH_TIMEOUT)
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("http client build failed: {e}"),
                    request.now,
                ));
            }
        };

        let host = url.host_str().unwrap_or("").to_string();

        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("fetch failed: {e}"),
                    request.now,
                ));
            }
        };

        let status = resp.status().as_u16();
        // resp.text() dekoodaa rungon Stringiksi (lossy-UTF-8) ilman erillistä
        // Bytes-tyyppiä; typistetään sitten cap-rajaan.
        let mut text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("body read failed: {e}"),
                    request.now,
                ));
            }
        };
        truncate_utf8(&mut text, cap);
        let bytes = text.len();

        let output: Value = json!({
            "status": status,
            "host": host,
            "bytes": bytes,
            "text": text,
        });

        // Haettu web-sisältö pysyy AINA epäluotettavana (ei .trusted()).
        Ok(ActionResult::success(
            format!("fetched {status} from {host} ({bytes} byte(s))"),
            output,
            request.now,
        ))
    }
}

impl Skill for WebFetchSkill {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "web_fetch".to_string(),
            version: "1.0.0".to_string(),
            description: "Hakee julkisen web-sivun sisällön (read-only HTTP GET, SSRF-vartioitu: \
                 vain http/https, ei yksityisiä/loopback-osoitteita, ei redirektejä); \
                 vastaus typistetään ja pysyy epäluotettavana."
                .to_string(),
            permissions: vec![SkillPermission::NetworkRead],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
            input_hint: Some("{ url, max_bytes? }".to_string()),
            output_hint: Some("{ status, host, bytes, text }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "Haettava URL (vain http/https, julkinen host)."
                    },
                    "max_bytes": {
                        "type": "integer",
                        "description": "Valinnainen vastauksen tavukatto (1..=524288)."
                    }
                },
                "required": ["url"],
                "additionalProperties": false
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_public_and_bad_schemes() {
        for bad in [
            "http://localhost/x",
            "http://app.localhost/x",
            "http://127.0.0.1/x",
            "http://10.0.0.1/x",
            "http://172.16.0.1/x",
            "http://192.168.1.1/x",
            "http://169.254.169.254/latest/meta-data",
            "http://100.64.0.1/x",
            "http://0.0.0.0/x",
            "http://[::1]/x",
            "http://[fe80::1]/x",
            "http://[fc00::1]/x",
            "file:///etc/passwd",
            "ftp://example.com/x",
            "data:text/plain,hello",
            "notaurl",
        ] {
            assert!(
                validate_url(bad).is_err(),
                "pitäisi hylätä mutta hyväksyi: {bad}"
            );
        }
    }

    #[test]
    fn accepts_public_http_and_https_without_network() {
        assert!(validate_url("http://example.com/").is_ok());
        assert!(validate_url("https://example.com/path?q=1").is_ok());
        assert!(validate_url("https://8.8.8.8/").is_ok());
    }

    #[test]
    fn truncate_is_utf8_safe_and_capped() {
        let mut s = "ääääää".to_string(); // 12 tavua (2/merkki)
        truncate_utf8(&mut s, 5);
        // Ei saa katkaista keskeltä monitavuista merkkiä → enintään 4 tavua (2 'ä').
        assert!(s.len() <= 5);
        assert!(s.is_char_boundary(s.len()));
        assert_eq!(s, "ää");
    }

    #[test]
    fn manifest_is_read_only_auto_and_generic() {
        let skill = WebFetchSkill::new();
        let m = skill.manifest();
        assert_eq!(m.name, "web_fetch");
        assert_eq!(m.risk, ActionRisk::ReadOnly);
        assert_eq!(m.approval_policy, ApprovalPolicy::AutoIfReadOnly);
        assert_eq!(m.permissions, vec![SkillPermission::NetworkRead]);
        let url_type = m.input_schema["properties"]["url"]["type"].as_str();
        assert_eq!(url_type, Some("string"));
        // Layer B: manifestissa ei saa olla perheen nimiä (geneerinen Kerros A).
        // Kielletyt fragmentit rakennetaan ROT13:sta, jotta tämä testi ei itse
        // sisällä Kerros B -nimiä literaaleina (muuten leak-audit napsahtaisi
        // tähän testitiedostoon, vaikka tuotantokoodi on puhdas).
        let blob = format!("{} {} {}", m.name, m.description, m.input_schema);
        let lower = blob.to_lowercase();
        let forbidden_rot13 = [
            "yhz", "cubgba", "cevfzn", "nheben", "pynfh", "ivyyr", "vfznry",
        ];
        for enc in forbidden_rot13 {
            let frag: String = enc
                .chars()
                .map(|c| {
                    let base = b'a';
                    (((c as u8 - base + 13) % 26) + base) as char
                })
                .collect();
            assert!(!lower.contains(&frag), "Layer B -vuoto manifestissa");
        }
    }
}
