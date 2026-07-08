//! Tutkimustaito: web-haku julkisen hakukoneen HTML-endpointin kautta (KERROS A).
//!
//! [`WebSearchSkill`] antaa agentille AIDON hakutyökalun — se lähettää
//! keyless-GET-pyynnön julkiseen hakukoneeseen (`DuckDuckGo` HTML-endpoint) ja
//! jäsentää tuloksista otsikot, `URLit` ja katkelmat. Tämä on tarkoituksella
//! **vain GET**, ei kirjoituksia, ja **rakenteellisesti SSRF-turvallinen**:
//!
//! ## Kuormaa kantava turvallisuus: kiinteä host + `validate_url`
//! Käyttäjän hakusyöte päätyy vain URL-query-parametriin (`?q=<query>`), EI
//! hostiin. Muodostettu URL osoittaa aina samaan kiinteään hakukone-hostiin, ja
//! se validoidaan silti `validate_url`illa ennen pyyntöä (sama SSRF-vartiointi
//! kuin `web_fetch`: vain http/https, ei localhost/yksityis-/loopback-/
//! link-local-osoitteita). Pyyntö EI seuraa redirectejä
//! ([`reqwest::redirect::Policy::none`]).
//!
//! ## Rajattu vastaus + rajatut tulokset
//! Vastausrunko typistetään (`RESPONSE_BYTE_CAP`), jottei valtava sivu syö
//! muistia, ja tulosten määrä rajataan (`DEFAULT_MAX_RESULTS`, kova katto
//! `HARD_MAX_RESULTS`).
//!
//! ## Taint (epäluotettavuus)
//! Haettu ja jäsennetty hakukonesisältö on AINA epäluotettavaa (taint) —
//! `execute` EI kutsu `.trusted()`. Verkosta tuotu sisältö ei pese itseään
//! puhtaaksi.

use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{ActionError, Result};
use crate::executor::{ActionExecutor, ActionRequest, ActionResult};
use crate::ids::SkillId;
use crate::manifest::SkillManifest;
use crate::policy::{ActionRisk, ApprovalPolicy, SkillPermission};

use super::Skill;

/// Taidon kiinteä tunniste (1-6 ovat varattuja muille oletustaidoille).
const SKILL_UUID: uuid::Uuid = uuid::uuid!("77777777-7777-4777-8777-777777777777");

/// Kiinteä hakukone-endpoint (keyless, julkinen GET). Käyttäjän query lisätään
/// vain `?q=`-parametrina — host EI koskaan tule syötteestä.
const SEARCH_ENDPOINT: &str = "https://html.duckduckgo.com/html/";

/// Tulosten oletusmäärä.
const DEFAULT_MAX_RESULTS: usize = 5;

/// Tulosten kova yläraja.
const HARD_MAX_RESULTS: usize = 20;

/// Vastausrungon kova tavukatto (512 KiB) — estää muistin syömisen valtavalla
/// sivulla.
const RESPONSE_BYTE_CAP: usize = 512 * 1024;

/// Verkkopyynnön aikakatkaisu.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Taidon syöte: hakusana ja valinnainen tulosten enimmäismäärä.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchInput {
    /// Hakusana (ei saa olla tyhjä eikä pelkkää välilyöntiä).
    pub query: String,
    /// Valinnainen tulosten enimmäismäärä (rajataan välille 1..=`HARD_MAX_RESULTS`).
    #[serde(default)]
    pub max_results: Option<usize>,
}

/// Yksittäinen hakutulos: otsikko, URL ja katkelma.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    /// Tuloksen otsikko (HTML-purettu, typistetty).
    pub title: String,
    /// Tuloksen kohde-URL.
    pub url: String,
    /// Lyhyt katkelma (HTML-purettu, typistetty).
    pub snippet: String,
}

/// Read-only web-haku -taito SSRF-vartioinnilla.
#[derive(Debug, Default, Clone)]
pub struct WebSearchSkill;

impl WebSearchSkill {
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

/// Rakentaa hakukone-URLin kiinteästä hostista + käyttäjän querystä.
///
/// Query päätyy vain `q`-parametriksi (URL-enkoodattuna [`reqwest::Url`]in
/// query-serialisoinnin kautta), joten se EI voi vaikuttaa hostiin, skeemaan
/// eikä polkuun. Tyhjä/whitespace-query hylätään.
///
/// # Errors
/// - [`ActionError::PolicyDenied`] jos query on tyhjä/whitespace, tai jos
///   kiinteä endpoint ei jostain syystä parsiudu (ei tapahdu käytännössä).
fn build_search_url(query: &str) -> Result<reqwest::Url> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(ActionError::PolicyDenied(
            "tyhjä hakusana ei sallittu (hylätty)".to_string(),
        ));
    }

    let mut url = reqwest::Url::parse(SEARCH_ENDPOINT)
        .map_err(|e| ActionError::PolicyDenied(format!("epäkelpo endpoint (hylätty): {e}")))?;
    // query_pairs_mut URL-enkoodaa arvon → käyttäjän teksti ei voi murtautua
    // ulos query-osasta hostiin/polkuun.
    url.query_pairs_mut().append_pair("q", trimmed);
    Ok(url)
}

/// Validoi `URLin` SSRF-turvallisesti. PUHDAS funktio — EI tee verkkopyyntöä.
///
/// Sama vartiointityyli kuin `web_fetch::validate_url`: hylkää ei-http/https-
/// skeemat, puuttuvan hostin, `localhost`-hostit ja ei-julkiset IP-osoitteet
/// (loopback/yksityinen/link-local/CGNAT/unspecified). Vaikka host tulee
/// kiinteästä vakiosta, tämä ajetaan silti puolustuksena syvyydessä.
///
/// # Errors
/// Palauttaa [`ActionError::PolicyDenied`] jos URL osoittaa ei-julkiseen
/// kohteeseen tai käyttää kiellettyä skeemaa.
fn validate_url(url: &reqwest::Url) -> Result<()> {
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

    // IPv6-host tulee hakasulkeissa (esim. "[::1]") — riisutaan ne ennen parsea,
    // muuten IpAddr::parse epäonnistuu ja loopback/link-local pääsisi läpi.
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

    Ok(())
}

/// Onko IP julkinen (ei loopback/yksityinen/link-local/CGNAT/unspecified)?
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() {
                return false;
            }
            // CGNAT 100.64.0.0/10.
            let o = v4.octets();
            if o[0] == 100 && (64..=127).contains(&o[1]) {
                return false;
            }
            !v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return false;
            }
            let seg = v6.segments();
            // fc00::/7 (unique local).
            if (seg[0] & 0xfe00) == 0xfc00 {
                return false;
            }
            // fe80::/10 (link local).
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

/// Enimmäispituus otsikolle/katkelmalle tavuina (pidetään todiste tiiviinä).
const FIELD_MAX_BYTES: usize = 400;

/// Purkaa yksinkertaiset HTML-entiteetit ja poistaa tagit annetusta pätkästä.
///
/// Tarkoituksella kevyt (ei HTML-parser-dependency): riisuu `<...>`-tagit,
/// dekoodaa yleisimmät entiteetit, tiivistää välit ja typistää.
fn clean_html_fragment(raw: &str) -> String {
    // Poista tagit: kaikki `<...>` korvataan välilyönnillä.
    let mut without_tags = String::with_capacity(raw.len());
    let mut in_tag = false;
    for c in raw.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                without_tags.push(' ');
            }
            _ if !in_tag => without_tags.push(c),
            _ => {}
        }
    }

    let decoded = without_tags
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");

    // Tiivistä toistuvat välit yhdeksi ja trimmaa.
    let mut collapsed = String::with_capacity(decoded.len());
    let mut prev_space = false;
    for c in decoded.chars() {
        if c.is_whitespace() {
            if !prev_space {
                collapsed.push(' ');
            }
            prev_space = true;
        } else {
            collapsed.push(c);
            prev_space = false;
        }
    }
    let mut out = collapsed.trim().to_string();
    truncate_utf8(&mut out, FIELD_MAX_BYTES);
    out
}

/// `DuckDuckGo` HTML-endpoint kääntää kohde-URLin uudelleenohjaus-linkiksi
/// muotoa `//duckduckgo.com/l/?uddg=<url-enkoodattu-kohde>&...`. Puretaan
/// oikea kohde-URL uddg-parametrista jos se löytyy; muuten palautetaan
/// alkuperäinen (normalisoituna).
fn decode_result_url(raw: &str) -> String {
    let candidate = raw.trim();
    // Etsi uddg-parametri.
    if let Some(idx) = candidate.find("uddg=") {
        let after = &candidate[idx + "uddg=".len()..];
        let encoded: String = after.chars().take_while(|&c| c != '&').collect();
        if let Ok(decoded) = percent_decode(&encoded) {
            if !decoded.is_empty() {
                return decoded;
            }
        }
    }
    // Normalisoi protokollaton `//host/...` → `https://host/...`.
    if let Some(rest) = candidate.strip_prefix("//") {
        return format!("https://{rest}");
    }
    candidate.to_string()
}

/// Minimaalinen percent-decode (`%XX` → tavu, `+` säilytetään). Riittää DDG:n
/// uddg-parametrin purkuun ilman ulkoista dependencyä.
///
/// # Errors
/// Palauttaa `()`-virheen jos `%XX` on epäkelpo tai dekoodattu tavujono ei ole
/// kelvollista UTF-8:aa.
fn percent_decode(s: &str) -> std::result::Result<String, ()> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                if i + 2 >= bytes.len() {
                    return Err(());
                }
                let hi = (bytes[i + 1] as char).to_digit(16).ok_or(())?;
                let lo = (bytes[i + 2] as char).to_digit(16).ok_or(())?;
                // hi ja lo ovat 0..=15, joten hi*16+lo on 0..=255 → mahtuu u8:aan.
                let byte = u8::try_from(hi * 16 + lo).map_err(|_| ())?;
                out.push(byte);
                i += 3;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| ())
}

/// Jäsentää `DuckDuckGo` HTML-endpointin vastauksesta hakutulokset.
///
/// Kevyt merkkijono-skannaus (ei HTML-parser-dependencyä): etsii
/// `result__a`-ankkurit (otsikko + href) ja niitä seuraavat
/// `result__snippet`-katkelmat. Palauttaa enintään `max_results` tulosta.
/// Jäsennetty sisältö on epäluotettavaa (taint säilyy kutsujalla).
fn parse_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    // Skannaa result-ankkurit. DDG HTML: <a ... class="result__a" href="...">OTSIKKO</a>
    let anchor_marker = "result__a";
    let mut search_from = 0;

    while results.len() < max_results {
        let Some(rel) = html[search_from..].find(anchor_marker) else {
            break;
        };
        let marker_idx = search_from + rel;

        // Löydä ankkurin alku (<a ... ennen markeria) ja href.
        let anchor_start = html[..marker_idx].rfind("<a").unwrap_or(marker_idx);
        // Etsi href="..." tästä ankkurista.
        let tag_region_end = html[anchor_start..]
            .find('>')
            .map_or(html.len(), |e| anchor_start + e);
        let tag_region = &html[anchor_start..tag_region_end];
        let href = extract_attr(tag_region, "href").unwrap_or_default();

        // Otsikkoteksti: ankkurin `>` jälkeen seuraavaan `</a>`.
        let title = if tag_region_end < html.len() {
            let text_start = tag_region_end + 1;
            let text_end = html[text_start..]
                .find("</a>")
                .map_or(html.len(), |e| text_start + e);
            clean_html_fragment(&html[text_start..text_end])
        } else {
            String::new()
        };

        // Katkelma: seuraava `result__snippet` markerin jälkeen (jos on ennen
        // seuraavaa result__a-ankkuria).
        let snippet = extract_snippet_after(html, tag_region_end);

        let url = decode_result_url(&href);
        if !title.is_empty() || !url.is_empty() {
            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }

        // Etene markerin ohi, jottei sama ankkuri jäsenny uudelleen.
        search_from = marker_idx + anchor_marker.len();
    }

    results
}

/// Poimii `attr="..."`-arvon tag-pätkästä (yksinkertainen, lainausmerkkien
/// väliin). Palauttaa `None` jos attribuuttia ei löydy.
fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Etsii `result__snippet`-katkelman annetusta kohdasta eteenpäin ja purkaa sen
/// tekstin. Palauttaa tyhjän merkkijonon jos katkelmaa ei löydy kohtuullisen
/// ikkunan sisältä.
fn extract_snippet_after(html: &str, from: usize) -> String {
    let marker = "result__snippet";
    let Some(rel) = html[from..].find(marker) else {
        return String::new();
    };
    let marker_idx = from + rel;
    // Katkelman teksti alkaa markerin sisältävän tagin `>` jälkeen.
    let Some(tag_close_rel) = html[marker_idx..].find('>') else {
        return String::new();
    };
    let text_start = marker_idx + tag_close_rel + 1;
    // Loppuu seuraavaan `</a>` tai `</div>`.
    let a_end = html[text_start..].find("</a>");
    let div_end = html[text_start..].find("</div>");
    let text_end = match (a_end, div_end) {
        (Some(a), Some(d)) => text_start + a.min(d),
        (Some(a), None) => text_start + a,
        (None, Some(d)) => text_start + d,
        (None, None) => html.len(),
    };
    clean_html_fragment(&html[text_start..text_end])
}

#[async_trait]
impl ActionExecutor for WebSearchSkill {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let input: WebSearchInput = match serde_json::from_value(request.payload.clone()) {
            Ok(input) => input,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid web_search input: {e}"),
                    request.now,
                ));
            }
        };

        // Rakenna hakukone-URL kiinteästä hostista + query-parametrista.
        // Tyhjä/whitespace-query hylätään tässä (fail-closed).
        let url = match build_search_url(&input.query) {
            Ok(url) => url,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("query rejected: {e}"),
                    request.now,
                ));
            }
        };

        // SSRF-vartiointi ENNEN pyyntöä (puolustus syvyydessä).
        if let Err(e) = validate_url(&url) {
            return Ok(ActionResult::failure(
                format!("url rejected: {e}"),
                request.now,
            ));
        }

        let max_results = input
            .max_results
            .unwrap_or(DEFAULT_MAX_RESULTS)
            .clamp(1, HARD_MAX_RESULTS);

        // EI redirektejä + aikakatkaisu. User-Agent asetetaan, koska DDG:n
        // HTML-endpoint voi muuten palauttaa tyhjän rungon.
        let client = match reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(FETCH_TIMEOUT)
            .user_agent("Mozilla/5.0 (compatible; familyclaw-web-search/1.0)")
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
                    format!("search request failed: {e}"),
                    request.now,
                ));
            }
        };

        let status = resp.status().as_u16();
        let mut body = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("body read failed: {e}"),
                    request.now,
                ));
            }
        };
        // Typistä runko ennen jäsennystä (muistiraja).
        truncate_utf8(&mut body, RESPONSE_BYTE_CAP);

        let results = parse_results(&body, max_results);
        let count = results.len();

        let results_json: Vec<Value> = results
            .iter()
            .map(|r| {
                json!({
                    "title": r.title,
                    "url": r.url,
                    "snippet": r.snippet,
                })
            })
            .collect();

        let output: Value = json!({
            "results": results_json,
            "count": count,
        });

        // Haettu hakukonesisältö pysyy AINA epäluotettavana (ei .trusted()).
        Ok(ActionResult::success(
            format!("search on {host} returned {count} result(s) (http {status})"),
            output,
            request.now,
        ))
    }
}

impl Skill for WebSearchSkill {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "web_search".to_string(),
            version: "1.0.0".to_string(),
            description: "Suorittaa web-haun julkisen hakukoneen HTML-endpointin kautta \
                 (read-only HTTP GET, keyless; SSRF-vartioitu: kiinteä host, vain http/https, \
                 ei redirektejä); jäsentää otsikot/URLit/katkelmat ja pysyy epäluotettavana."
                .to_string(),
            permissions: vec![SkillPermission::NetworkRead],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
            input_hint: Some("{ query, max_results? }".to_string()),
            output_hint: Some("{ results: [{ title, url, snippet }], count }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Hakusana (ei tyhjä)."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Valinnainen tulosten enimmäismäärä (1..=20)."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            publisher: None,
            signature: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pieni upotettu HTML-näyte joka jäljittelee `DuckDuckGo` HTML-endpointin
    /// tulosrakennetta (`result__a` -ankkurit + `result__snippet` -katkelmat).
    const SAMPLE_HTML: &str = r#"
<html><body>
<div class="result results_links">
  <div class="links_main">
    <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Ffirst&amp;rut=abc">First &amp; Best Result</a>
    <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Ffirst">This is the <b>first</b> snippet describing the page.</a>
  </div>
</div>
<div class="result results_links">
  <div class="links_main">
    <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Fsecond&amp;rut=def">Second Result Title</a>
    <a class="result__snippet" href="x">Second snippet with &lt;details&gt; here.</a>
  </div>
</div>
</body></html>
"#;

    #[test]
    fn build_search_url_encodes_query_into_q_param() {
        let url = build_search_url("rust async traits").expect("builds");
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("html.duckduckgo.com"));
        assert_eq!(url.path(), "/html/");
        // Query on q-parametrissa, välit enkoodattu (ei hostissa/polussa).
        let q = url
            .query_pairs()
            .find(|(k, _)| k == "q")
            .map(|(_, v)| v.into_owned());
        assert_eq!(q, Some("rust async traits".to_string()));
        // Host EI muuttunut käyttäjän syötteestä.
        assert_eq!(url.host_str(), Some("html.duckduckgo.com"));
    }

    #[test]
    fn build_search_url_trims_and_rejects_empty() {
        assert!(build_search_url("").is_err());
        assert!(build_search_url("   ").is_err());
        assert!(build_search_url("\t\n ").is_err());
        // Reunat trimmataan mutta sisältö säilyy.
        let url = build_search_url("  hello  ").expect("builds");
        let q = url
            .query_pairs()
            .find(|(k, _)| k == "q")
            .map(|(_, v)| v.into_owned());
        assert_eq!(q, Some("hello".to_string()));
    }

    #[test]
    fn validate_url_accepts_fixed_endpoint_and_rejects_private() {
        let ok = build_search_url("x").expect("builds");
        assert!(validate_url(&ok).is_ok());

        for bad in [
            "http://localhost/html/?q=x",
            "http://127.0.0.1/html/?q=x",
            "http://169.254.169.254/html/?q=x",
            "ftp://example.com/?q=x",
        ] {
            let url = reqwest::Url::parse(bad).expect("parse test url");
            assert!(validate_url(&url).is_err(), "should reject: {bad}");
        }
    }

    #[test]
    fn parse_results_extracts_entries_from_sample() {
        let results = parse_results(SAMPLE_HTML, 10);
        assert!(
            !results.is_empty(),
            "must extract at least one result from sample HTML"
        );
        assert_eq!(results.len(), 2, "sample has exactly two results");

        // Ensimmäinen tulos: otsikko HTML-purettu, URL uddg-parametrista.
        assert_eq!(results[0].title, "First & Best Result");
        assert_eq!(results[0].url, "https://example.com/first");
        assert!(
            results[0].snippet.contains("first snippet"),
            "snippet parsed: {:?}",
            results[0].snippet
        );

        // Toinen tulos: otsikko + dekoodattu URL.
        assert_eq!(results[1].title, "Second Result Title");
        assert_eq!(results[1].url, "https://example.org/second");
        assert!(results[1].snippet.contains("<details>"));
    }

    #[test]
    fn parse_results_respects_max_results_cap() {
        let one = parse_results(SAMPLE_HTML, 1);
        assert_eq!(one.len(), 1, "must cap results to max_results");
    }

    #[test]
    fn parse_results_empty_on_no_matches() {
        let results = parse_results("<html><body>no results here</body></html>", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn clean_html_fragment_strips_tags_and_decodes_entities() {
        let cleaned = clean_html_fragment("Hello <b>world</b> &amp; goodbye");
        assert_eq!(cleaned, "Hello world & goodbye");
    }

    #[test]
    fn percent_decode_roundtrips_url() {
        let decoded = percent_decode("https%3A%2F%2Fexample.com%2Fa").expect("decodes");
        assert_eq!(decoded, "https://example.com/a");
        // Epäkelpo %XX hylätään.
        assert!(percent_decode("%ZZ").is_err());
        assert!(percent_decode("%2").is_err());
    }

    #[tokio::test]
    async fn empty_query_rejected_by_execute() {
        use crate::ids::{ActionId, ActionTaskId};
        use familyclaw_core::time::from_unix_secs;

        let skill = WebSearchSkill::new();
        let payload = serde_json::to_value(WebSearchInput {
            query: "   ".to_string(),
            max_results: None,
        })
        .expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            WebSearchSkill::skill_id(),
            ActionTaskId::new(),
            payload,
            from_unix_secs(1_700_000_000).expect("valid unix seconds"),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(
            !res.status.is_success(),
            "whitespace-only query must be rejected (no network call)"
        );
        assert!(res.output_summary.contains("rejected"));
    }

    #[test]
    fn manifest_is_read_only_auto_and_generic() {
        let skill = WebSearchSkill::new();
        let m = skill.manifest();
        m.validate().expect("manifest validates");
        assert_eq!(m.name, "web_search");
        assert_eq!(m.risk, ActionRisk::ReadOnly);
        assert_eq!(m.approval_policy, ApprovalPolicy::AutoIfReadOnly);
        assert_eq!(m.permissions, vec![SkillPermission::NetworkRead]);
        assert_eq!(
            m.input_schema["properties"]["query"]["type"].as_str(),
            Some("string")
        );
        assert_eq!(m.input_schema["required"][0], "query");

        // Layer B: manifestissa ei saa olla perheen nimiä (geneerinen Kerros A).
        // Kielletyt fragmentit rakennetaan ROT13:sta, jotta tämä testi ei itse
        // sisällä Kerros B -nimiä literaaleina.
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
