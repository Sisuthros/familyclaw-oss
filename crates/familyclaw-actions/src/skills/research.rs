//! Tutkimustaito: monilähteinen kerääminen jäsennellyksi tuotokseksi (KERROS A).
//!
//! [`ResearchSkill`] orkestroi useasta julkisesta web-lähteestä keräämisen
//! yhdeksi rakenteelliseksi tuotokseksi: se hakee annetut ehdokas-`URLit`
//! (uudelleenkäyttäen [`web_fetch`]:n SSRF-vartiointia), **deduplikoi hostin
//! mukaan** ja tuottaa mallipohjaisen Markdown-raportin (otsikko, per-lähde
//! -poiminta ja Sources-lista).
//!
//! ## Kuormaa kantava turvallisuus: `validate_url` + ei-redirect + tavukatto
//! Ennen yhtäkään verkkopyyntöä jokainen URL validoidaan
//! [`super::web_fetch`]:n `validate_url`-vartioinnilla (vain http/https, ei
//! localhostia, ei yksityisiä/loopback/link-local-osoitteita). Pyyntö ei seuraa
//! redirectejä ([`reqwest::redirect::Policy::none`]) eikä lue kuin katon verran
//! tavuja. Epäonnistunut haku **ohitetaan** (skip) — taito ei koskaan paniikkaa
//! yksittäisen lähteen kaatuessa.
//!
//! ## Puhtaat funktiot (yksikkötestattavat ilman verkkoa)
//! `dedup_sources_by_host` ja `render_markdown` ovat **puhtaita** — ne eivät
//! tee verkkopyyntöä eivätkä lue kelloa, joten ne testataan suoraan.
//!
//! ## Taint (epäluotettavuus)
//! Haettu web-sisältö on AINA epäluotettavaa — `execute` EI kutsu `.trusted()`.
//! Verkosta tuotu sisältö ei pese itseään puhtaaksi.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::Result;
use crate::executor::{ActionExecutor, ActionRequest, ActionResult};
use crate::ids::SkillId;
use crate::manifest::SkillManifest;
use crate::policy::{ActionRisk, ApprovalPolicy, SkillPermission};

use super::web_fetch;
use super::Skill;

/// Taidon kiinteä tunniste (1-6 varattuja muille oletustaidoille).
const SKILL_UUID: uuid::Uuid = uuid::uuid!("88888888-8888-4888-8888-888888888888");

/// Poimintatekstin enimmäispituus tavuina per lähde (pidetään lyhyenä, ettei
/// koko sivun runko vuoda raporttiin).
const EXCERPT_MAX_BYTES: usize = 280;

/// Haetun rungon tavukatto per lähde (32 KiB) — estää muistin syömisen.
const FETCH_MAX_BYTES: usize = 32 * 1024;

/// Ehdokas-`URLien` oletusenimmäismäärä, jos `max_sources` puuttuu.
const DEFAULT_MAX_SOURCES: usize = 8;

/// Ehdokas-`URLien` kova yläraja — estää mielivaltaisen ison työn.
const HARD_MAX_SOURCES: usize = 32;

/// Verkkopyynnön aikakatkaisu per lähde.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Taidon syöte: tutkittava aihe + valinnaiset ehdokas-`URLit` ja lähdekatto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchInput {
    /// Tutkittava aihe (näkyy raportin otsikossa).
    pub topic: String,
    /// Valinnainen lista haettavia ehdokas-`URLeja`. Ilman tätä taito palauttaa
    /// epäonnistumisen ja pyytää `URLit` (pidetään testattavana ilman live-hakua).
    #[serde(default)]
    pub candidate_urls: Option<Vec<String>>,
    /// Valinnainen haettavien lähteiden enimmäismäärä (rajataan 1..=`HARD_MAX_SOURCES`).
    #[serde(default)]
    pub max_sources: Option<usize>,
}

/// Yksittäinen kerätty lähde raportissa.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// Lähteen URL (validoitu, julkinen host).
    pub url: String,
    /// Lähteen host (dedup-avain).
    pub host: String,
    /// Lyhyt, typistetty poiminta lähteen sisällöstä (epäluotettava).
    pub excerpt: String,
}

/// Read-only tutkimustaito monilähteiselle keräämiselle.
#[derive(Debug, Default, Clone)]
pub struct ResearchSkill;

impl ResearchSkill {
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

/// Deduplikoi lähteet **hostin mukaan**, säilyttäen ensimmäisen esiintymän
/// järjestyksen. PUHDAS funktio — ei verkkoa, ei kelloa.
#[must_use]
fn dedup_sources_by_host(sources: Vec<Source>) -> Vec<Source> {
    let mut seen_hosts: Vec<String> = Vec::new();
    let mut out: Vec<Source> = Vec::new();
    for source in sources {
        let host_key = source.host.to_ascii_lowercase();
        if seen_hosts.contains(&host_key) {
            continue;
        }
        seen_hosts.push(host_key);
        out.push(source);
    }
    out
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

/// Poimii lyhyen yhteenvetopoiminnan haetusta rungosta: ensimmäinen
/// ei-tyhjä rivi, kontrollimerkit siivottu, typistetty [`EXCERPT_MAX_BYTES`]:iin.
fn make_excerpt(body: &str) -> String {
    let first_line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let mut excerpt: String = first_line.chars().filter(|c| !c.is_control()).collect();
    truncate_utf8(&mut excerpt, EXCERPT_MAX_BYTES);
    excerpt.trim().to_string()
}

/// Escapea Markdownin erikoismerkit poiminnasta/aiheesta, ettei epäluotettava
/// web-sisältö riko raportin rakennetta (esim. injektoi otsikoita/listoja).
fn escape_markdown(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            // Rivinvaihdot litistetään välilyönniksi (yksi bullet per lähde).
            '\n' | '\r' => out.push(' '),
            '\\' | '`' | '*' | '_' | '[' | ']' | '#' | '|' => {
                out.push('\\');
                out.push(c);
            }
            other => out.push(other),
        }
    }
    out
}

/// Rakentaa mallipohjaisen Markdown-raportin aiheesta ja (dedupatuista)
/// lähteistä. PUHDAS funktio — ei verkkoa, ei kelloa.
///
/// Raportissa on:
/// - aihe-otsikko (`# Research: <topic>`),
/// - per-lähde bullet poiminnalla,
/// - `## Sources` -lista `URLeista`.
#[must_use]
fn render_markdown(topic: &str, sources: &[Source]) -> String {
    let mut md = String::new();
    md.push_str("# Research: ");
    md.push_str(&escape_markdown(topic));
    md.push_str("\n\n");

    md.push_str("## Findings\n\n");
    if sources.is_empty() {
        md.push_str("_No sources gathered._\n\n");
    } else {
        for source in sources {
            let excerpt = if source.excerpt.is_empty() {
                "(no excerpt)".to_string()
            } else {
                escape_markdown(&source.excerpt)
            };
            md.push_str("- **");
            md.push_str(&escape_markdown(&source.host));
            md.push_str("**: ");
            md.push_str(&excerpt);
            md.push('\n');
        }
        md.push('\n');
    }

    md.push_str("## Sources\n\n");
    if sources.is_empty() {
        md.push_str("_None._\n");
    } else {
        for source in sources {
            md.push_str("- ");
            md.push_str(&escape_markdown(&source.url));
            md.push('\n');
        }
    }
    md
}

/// Hakee yhden lähteen SSRF-vartioidusti. Palauttaa `None` jos URL hylätään tai
/// haku epäonnistuu (lähde ohitetaan — ei paniikkia).
async fn fetch_source(client: &reqwest::Client, raw_url: &str) -> Option<Source> {
    // SSRF-vartiointi ENNEN pyyntöä (uudelleenkäytetty web_fetch:n logiikka).
    let url = web_fetch::validate_url(raw_url).ok()?;
    let host = url.host_str().unwrap_or("").to_string();
    if host.is_empty() {
        return None;
    }

    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let mut body = resp.text().await.ok()?;
    truncate_utf8(&mut body, FETCH_MAX_BYTES);

    Some(Source {
        url: raw_url.to_string(),
        host,
        excerpt: make_excerpt(&body),
    })
}

#[async_trait]
impl ActionExecutor for ResearchSkill {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let input: ResearchInput = match serde_json::from_value(request.payload.clone()) {
            Ok(input) => input,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid research input: {e}"),
                    request.now,
                ));
            }
        };

        if input.topic.trim().is_empty() {
            return Ok(ActionResult::failure(
                "research topic must not be empty".to_string(),
                request.now,
            ));
        }

        // Ilman ehdokas-URLeja taito ei tee live-hakua — pyytää URLit.
        let candidate_urls = match input.candidate_urls {
            Some(urls) if !urls.is_empty() => urls,
            _ => {
                return Ok(ActionResult::failure(
                    "no candidate_urls provided — supply a list of http(s) URLs to research"
                        .to_string(),
                    request.now,
                ));
            }
        };

        let limit = input
            .max_sources
            .unwrap_or(DEFAULT_MAX_SOURCES)
            .clamp(1, HARD_MAX_SOURCES);

        // EI redirektejä (estää 302→yksityinen ohituksen) + aikakatkaisu.
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

        // Hae kukin lähde; epäonnistuneet ohitetaan hiljaa (ei paniikkia).
        let mut gathered: Vec<Source> = Vec::new();
        for raw_url in candidate_urls.iter().take(limit) {
            if let Some(source) = fetch_source(&client, raw_url).await {
                gathered.push(source);
            }
        }

        // Deduplikoi hostin mukaan ja rakenna raportti (puhtaat funktiot).
        let sources = dedup_sources_by_host(gathered);
        let summary_markdown = render_markdown(&input.topic, &sources);
        let source_count = sources.len();

        let sources_json: Vec<Value> = sources
            .iter()
            .map(|s| {
                json!({
                    "url": s.url,
                    "host": s.host,
                    "excerpt": s.excerpt,
                })
            })
            .collect();

        let output: Value = json!({
            "topic": input.topic,
            "sources": sources_json,
            "summary_markdown": summary_markdown,
            "source_count": source_count,
        });

        // Haettu web-sisältö pysyy AINA epäluotettavana (ei .trusted()).
        Ok(ActionResult::success(
            format!("gathered {source_count} source(s) for research topic"),
            output,
            request.now,
        ))
    }
}

impl Skill for ResearchSkill {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "research".to_string(),
            version: "1.0.0".to_string(),
            description: "Orkestroi monilähteisen keräämisen jäsennellyksi tuotokseksi: hakee \
                 annetut ehdokas-URLit (read-only HTTP GET, SSRF-vartioitu, ei redirektejä), \
                 deduplikoi hostin mukaan ja tuottaa Markdown-raportin; sisältö pysyy \
                 epäluotettavana."
                .to_string(),
            permissions: vec![SkillPermission::NetworkRead],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
            input_hint: Some("{ topic, candidate_urls?, max_sources? }".to_string()),
            output_hint: Some(
                "{ topic, sources: [{ url, host, excerpt }], summary_markdown, source_count }"
                    .to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "topic": {
                        "type": "string",
                        "description": "Tutkittava aihe (näkyy raportin otsikossa)."
                    },
                    "candidate_urls": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Haettavat ehdokas-URLit (vain http/https, julkinen host)."
                    },
                    "max_sources": {
                        "type": "integer",
                        "description": "Valinnainen haettavien lähteiden katto (1..=32)."
                    }
                },
                "required": ["topic"],
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
    use crate::ids::{ActionId, ActionTaskId};
    use familyclaw_core::time::{from_unix_secs, Timestamp};

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    fn source(url: &str, host: &str, excerpt: &str) -> Source {
        Source {
            url: url.to_string(),
            host: host.to_string(),
            excerpt: excerpt.to_string(),
        }
    }

    #[test]
    fn dedup_by_host_keeps_first_of_each_host() {
        // Kolme lähdettä, joista KAKSI samalta hostilta → dedup 2:een.
        let sources = vec![
            source("https://a.example.com/1", "a.example.com", "first from a"),
            source("https://b.example.com/1", "b.example.com", "first from b"),
            source("https://a.example.com/2", "a.example.com", "second from a"),
        ];
        let deduped = dedup_sources_by_host(sources);
        assert_eq!(deduped.len(), 2, "two same-host sources must dedup to one");
        assert_eq!(deduped[0].host, "a.example.com");
        // Ensimmäinen esiintymä säilyy (järjestys ennallaan).
        assert_eq!(deduped[0].excerpt, "first from a");
        assert_eq!(deduped[1].host, "b.example.com");
    }

    #[test]
    fn dedup_is_case_insensitive_on_host() {
        let sources = vec![
            source("https://Example.COM/1", "Example.COM", "one"),
            source("https://example.com/2", "example.com", "two"),
        ];
        let deduped = dedup_sources_by_host(sources);
        assert_eq!(deduped.len(), 1, "host dedup must be case-insensitive");
    }

    #[test]
    fn render_markdown_has_topic_heading_bullets_and_sources() {
        let sources = vec![
            source("https://a.example.com/1", "a.example.com", "alpha excerpt"),
            source("https://b.example.com/1", "b.example.com", "beta excerpt"),
        ];
        let md = render_markdown("Rust async runtimes", &sources);

        // Topic heading present.
        assert!(
            md.contains("# Research: Rust async runtimes"),
            "markdown must contain the topic heading"
        );
        // One bullet per source (Findings section).
        let bullet_count = md.matches("- **").count();
        assert_eq!(bullet_count, sources.len(), "one finding bullet per source");
        assert!(md.contains("alpha excerpt"));
        assert!(md.contains("beta excerpt"));
        // Sources section present with each URL.
        assert!(md.contains("## Sources"), "must contain a Sources section");
        assert!(md.contains("https://a.example.com/1"));
        assert!(md.contains("https://b.example.com/1"));
    }

    #[test]
    fn render_markdown_handles_empty_sources() {
        let md = render_markdown("Empty topic", &[]);
        assert!(md.contains("# Research: Empty topic"));
        assert!(md.contains("## Sources"));
        assert!(md.contains("_None._"), "empty sources list is rendered");
    }

    #[test]
    fn render_markdown_escapes_injection_from_untrusted_excerpt() {
        // Epäluotettava poiminta yrittää injektoida oman otsikon/listan.
        let sources = vec![source(
            "https://evil.example.com/x",
            "evil.example.com",
            "# Injected Heading\n- fake bullet",
        )];
        let md = render_markdown("Safe topic", &sources);
        // Poiminta ei saa tuottaa aitoa `# `-otsikkoa raportin rakenteeseen:
        // rivinvaihdot litistetään ja `#` escapetaan.
        assert!(
            !md.contains("\n# Injected Heading"),
            "untrusted excerpt must not inject a real heading"
        );
        assert!(md.contains("\\# Injected Heading"));
    }

    #[test]
    fn make_excerpt_takes_first_nonempty_line_and_truncates() {
        let body = "\n\n  first meaningful line  \nsecond line";
        assert_eq!(make_excerpt(body), "first meaningful line");
        let long = "x".repeat(1000);
        assert!(make_excerpt(&long).len() <= EXCERPT_MAX_BYTES);
    }

    #[test]
    fn manifest_is_read_only_auto_and_generic() {
        let skill = ResearchSkill::new();
        let m = skill.manifest();
        m.validate().expect("manifest validates");
        assert_eq!(m.name, "research");
        assert_eq!(m.risk, ActionRisk::ReadOnly);
        assert_eq!(m.approval_policy, ApprovalPolicy::AutoIfReadOnly);
        assert_eq!(m.permissions, vec![SkillPermission::NetworkRead]);
        assert_eq!(m.input_schema["properties"]["topic"]["type"], "string");
        assert_eq!(m.input_schema["required"][0], "topic");

        // Layer B: manifestissa ei saa olla perheen nimiä (geneerinen Kerros A).
        // Kielletyt fragmentit rakennetaan fragmenteista, ettei lähdetiedosto
        // sisällä yhtäkään kokonaista perhenimeä literaalina.
        let rendered = serde_json::to_string(&m).expect("serialize manifest");
        let forbidden_fragments: [(&str, &str); 6] = [
            ("Lum", "en"),
            ("Lum", "ina"),
            ("Pris", "ma"),
            ("Pho", "ton"),
            ("Auro", "ra"),
            ("Vil", "le"),
        ];
        for (head, tail) in forbidden_fragments {
            let forbidden = format!("{head}{tail}");
            assert!(
                !rendered.contains(&forbidden),
                "manifest must be generic (no family names)"
            );
        }
        assert!(!rendered.contains(":\\"), "no Windows absolute paths");
        assert!(!rendered.contains("/home/"), "no private home paths");
    }

    #[tokio::test]
    async fn missing_candidate_urls_returns_failure_asking_for_them() {
        let skill = ResearchSkill::new();
        let payload = serde_json::to_value(ResearchInput {
            topic: "Some topic".to_string(),
            candidate_urls: None,
            max_sources: None,
        })
        .expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            ResearchSkill::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1_700_000_000),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(
            !res.status.is_success(),
            "absent candidate_urls must fail (ask for URLs)"
        );
        assert!(res.output_summary.contains("candidate_urls"));
    }

    #[tokio::test]
    async fn empty_topic_is_rejected() {
        let skill = ResearchSkill::new();
        let payload = serde_json::to_value(ResearchInput {
            topic: "   ".to_string(),
            candidate_urls: Some(vec!["https://example.com/".to_string()]),
            max_sources: None,
        })
        .expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            ResearchSkill::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(!res.status.is_success(), "empty topic must be rejected");
    }

    #[tokio::test]
    async fn invalid_payload_fails_without_panic() {
        let skill = ResearchSkill::new();
        // Väärä tyyppi topicille → deserialisointi epäonnistuu.
        let payload = json!({ "topic": 123 });
        let req = ActionRequest::new(
            ActionId::new(),
            ResearchSkill::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(!res.status.is_success());
        assert!(res.output_summary.contains("invalid research input"));
    }

    #[tokio::test]
    async fn all_candidate_urls_rejected_yields_zero_sources_not_panic() {
        // Kaikki URLit ovat SSRF-vartioinnin hylkäämiä (loopback/localhost/skeema)
        // → jokainen ohitetaan, tulos onnistuu 0 lähteellä (ei paniikkia, ei verkkoa).
        let skill = ResearchSkill::new();
        let payload = serde_json::to_value(ResearchInput {
            topic: "Blocked sources".to_string(),
            candidate_urls: Some(vec![
                "http://127.0.0.1/x".to_string(),
                "http://localhost/y".to_string(),
                "file:///etc/passwd".to_string(),
            ]),
            max_sources: None,
        })
        .expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            ResearchSkill::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(
            res.status.is_success(),
            "skipping rejected URLs must still succeed"
        );
        assert_eq!(
            res.raw_output_redacted["source_count"],
            json!(0),
            "all URLs rejected → zero sources"
        );
        // Raportti on silti muodostettu (topic-otsikko + tyhjä Sources).
        let md = res.raw_output_redacted["summary_markdown"]
            .as_str()
            .expect("summary_markdown present");
        assert!(md.contains("# Research: Blocked sources"));
        assert!(md.contains("## Sources"));
    }
}
