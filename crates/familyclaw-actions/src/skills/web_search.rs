//! Research skill: web search via a public search engine's HTML endpoint (Layer A).
//!
//! [`WebSearchSkill`] gives the agent a GENUINE search tool — it sends a
//! keyless GET request to a public search engine (the `DuckDuckGo` HTML
//! endpoint) and parses the titles, `URLs`, and snippets out of the results.
//! This is intentionally **GET only**, no writes, and **structurally
//! SSRF-safe**:
//!
//! ## Load-bearing security: fixed host + `validate_url`
//! The user's search input ends up only in the URL query parameter
//! (`?q=<query>`), NOT in the host. The constructed URL always points to the
//! same fixed search-engine host, and it is still validated with
//! `validate_url` before the request (the same SSRF guard as `web_fetch`:
//! only http/https, no localhost/private/loopback/link-local addresses).
//! The request does NOT follow redirects
//! ([`reqwest::redirect::Policy::none`]).
//!
//! ## Bounded response + bounded results
//! The response body is truncated (`RESPONSE_BYTE_CAP`) so a huge page
//! cannot eat memory, and the number of results is bounded
//! (`DEFAULT_MAX_RESULTS`, hard cap `HARD_MAX_RESULTS`).
//!
//! ## Taint (untrustworthiness)
//! Fetched and parsed search-engine content is ALWAYS untrusted (tainted) —
//! `execute` does NOT call `.trusted()`. Content brought in from the network
//! cannot launder itself clean.

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

/// The skill's fixed identifier (1-6 are reserved for other default skills).
const SKILL_UUID: uuid::Uuid = uuid::uuid!("77777777-7777-4777-8777-777777777777");

/// Fixed search-engine endpoint (keyless, public GET). The user's query is
/// added only as a `?q=` parameter — the host NEVER comes from the input.
const SEARCH_ENDPOINT: &str = "https://html.duckduckgo.com/html/";

/// Default number of results.
const DEFAULT_MAX_RESULTS: usize = 5;

/// Hard upper bound on the number of results.
const HARD_MAX_RESULTS: usize = 20;

/// Hard byte cap on the response body (512 KiB) — prevents memory from
/// being eaten by a huge page.
const RESPONSE_BYTE_CAP: usize = 512 * 1024;

/// Timeout for the network request.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The skill's input: the search term and an optional maximum result count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchInput {
    /// Search term (must not be empty or whitespace-only).
    pub query: String,
    /// Optional maximum number of results (clamped to 1..=`HARD_MAX_RESULTS`).
    #[serde(default)]
    pub max_results: Option<usize>,
}

/// A single search result: title, URL, and snippet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    /// The result's title (HTML-stripped, truncated).
    pub title: String,
    /// The result's target URL.
    pub url: String,
    /// A short snippet (HTML-stripped, truncated).
    pub snippet: String,
}

/// Read-only web search skill with SSRF guarding.
#[derive(Debug, Default, Clone)]
pub struct WebSearchSkill;

impl WebSearchSkill {
    /// Creates a new skill instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// The skill's fixed identifier.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(SKILL_UUID)
    }
}

/// Builds the search-engine URL from the fixed host + the user's query.
///
/// The query ends up only as the `q` parameter (URL-encoded via
/// [`reqwest::Url`]'s query serialization), so it CANNOT affect the host,
/// scheme, or path. An empty/whitespace query is rejected.
///
/// # Errors
/// - [`ActionError::PolicyDenied`] if the query is empty/whitespace, or if
///   the fixed endpoint somehow fails to parse (does not happen in practice).
fn build_search_url(query: &str) -> Result<reqwest::Url> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(ActionError::PolicyDenied(
            "empty search term not allowed (rejected)".to_string(),
        ));
    }

    let mut url = reqwest::Url::parse(SEARCH_ENDPOINT)
        .map_err(|e| ActionError::PolicyDenied(format!("invalid endpoint (rejected): {e}")))?;
    // query_pairs_mut URL-encodes the value → the user's text cannot break
    // out of the query part into the host/path.
    url.query_pairs_mut().append_pair("q", trimmed);
    Ok(url)
}

/// Validates the `URL` for SSRF safety. A PURE function — makes NO network request.
///
/// Same guarding style as `web_fetch::validate_url`: rejects non-http/https
/// schemes, a missing host, `localhost` hosts, and non-public IP addresses
/// (loopback/private/link-local/CGNAT/unspecified). Even though the host
/// comes from a fixed constant, this still runs as defense in depth.
///
/// # Errors
/// Returns [`ActionError::PolicyDenied`] if the URL points to a non-public
/// target or uses a forbidden scheme.
fn validate_url(url: &reqwest::Url) -> Result<()> {
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(ActionError::PolicyDenied(format!(
                "scheme '{other}' not allowed (http/https only; rejected)"
            )));
        }
    }

    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| ActionError::PolicyDenied("URL has no host (rejected)".to_string()))?;

    let host_lower = host.to_ascii_lowercase();
    if host_lower == "localhost" || host_lower.ends_with(".localhost") {
        return Err(ActionError::PolicyDenied(
            "localhost not allowed (rejected)".to_string(),
        ));
    }

    // An IPv6 host comes in brackets (e.g. "[::1]") — strip them before
    // parsing, otherwise IpAddr::parse fails and loopback/link-local would
    // slip through.
    let host_for_ip = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = host_for_ip.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            return Err(ActionError::PolicyDenied(format!(
                "non-public IP {ip} not allowed (rejected)"
            )));
        }
    }

    Ok(())
}

/// Whether the IP is public (not loopback/private/link-local/CGNAT/unspecified)?
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

/// Truncates a string to at most `max_bytes` bytes while preserving UTF-8 boundaries.
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

/// Maximum length for a title/snippet in bytes (keeps the proof compact).
const FIELD_MAX_BYTES: usize = 400;

/// Decodes simple HTML entities and strips tags from the given fragment.
///
/// Intentionally lightweight (no HTML-parser dependency): strips `<...>`
/// tags, decodes the most common entities, collapses whitespace, and
/// truncates.
fn clean_html_fragment(raw: &str) -> String {
    // Strip tags: every `<...>` is replaced with a space.
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

    // Collapse repeated whitespace into one and trim.
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

/// The `DuckDuckGo` HTML endpoint turns the target URL into a redirect link
/// of the form `//duckduckgo.com/l/?uddg=<url-encoded-target>&...`. Decodes
/// the real target URL from the uddg parameter if found; otherwise returns
/// the original (normalized).
fn decode_result_url(raw: &str) -> String {
    let candidate = raw.trim();
    // Look for the uddg parameter.
    if let Some(idx) = candidate.find("uddg=") {
        let after = &candidate[idx + "uddg=".len()..];
        let encoded: String = after.chars().take_while(|&c| c != '&').collect();
        if let Ok(decoded) = percent_decode(&encoded) {
            if !decoded.is_empty() {
                return decoded;
            }
        }
    }
    // Normalize a protocol-relative `//host/...` → `https://host/...`.
    if let Some(rest) = candidate.strip_prefix("//") {
        return format!("https://{rest}");
    }
    candidate.to_string()
}

/// Minimal percent-decode (`%XX` → byte, `+` preserved as-is). Sufficient
/// for decoding DDG's uddg parameter without an external dependency.
///
/// # Errors
/// Returns a `()` error if `%XX` is invalid or the decoded byte sequence is
/// not valid UTF-8.
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
                // hi and lo are 0..=15, so hi*16+lo is 0..=255 → fits in a u8.
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

/// Parses search results out of the `DuckDuckGo` HTML endpoint's response.
///
/// Lightweight string scanning (no HTML-parser dependency): looks for
/// `result__a` anchors (title + href) and the `result__snippet` fragments
/// that follow them. Returns at most `max_results` results. The parsed
/// content is untrusted (taint remains with the caller).
fn parse_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    // Scan for result anchors. DDG HTML: <a ... class="result__a" href="...">TITLE</a>
    let anchor_marker = "result__a";
    let mut search_from = 0;

    while results.len() < max_results {
        let Some(rel) = html[search_from..].find(anchor_marker) else {
            break;
        };
        let marker_idx = search_from + rel;

        // Find the anchor's start (<a ... before the marker) and its href.
        let anchor_start = html[..marker_idx].rfind("<a").unwrap_or(marker_idx);
        // Look for href="..." within this anchor.
        let tag_region_end = html[anchor_start..]
            .find('>')
            .map_or(html.len(), |e| anchor_start + e);
        let tag_region = &html[anchor_start..tag_region_end];
        let href = extract_attr(tag_region, "href").unwrap_or_default();

        // Title text: after the anchor's `>` up to the next `</a>`.
        let title = if tag_region_end < html.len() {
            let text_start = tag_region_end + 1;
            let text_end = html[text_start..]
                .find("</a>")
                .map_or(html.len(), |e| text_start + e);
            clean_html_fragment(&html[text_start..text_end])
        } else {
            String::new()
        };

        // Snippet: the next `result__snippet` after the marker (if it
        // appears before the next result__a anchor).
        let snippet = extract_snippet_after(html, tag_region_end);

        let url = decode_result_url(&href);
        if !title.is_empty() || !url.is_empty() {
            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }

        // Advance past the marker, so the same anchor is not parsed again.
        search_from = marker_idx + anchor_marker.len();
    }

    results
}

/// Extracts an `attr="..."` value from a tag fragment (simple, between
/// quotes). Returns `None` if the attribute is not found.
fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Looks for a `result__snippet` fragment starting from the given position
/// and extracts its text. Returns an empty string if no fragment is found
/// within a reasonable window.
fn extract_snippet_after(html: &str, from: usize) -> String {
    let marker = "result__snippet";
    let Some(rel) = html[from..].find(marker) else {
        return String::new();
    };
    let marker_idx = from + rel;
    // The snippet text starts after the `>` of the tag containing the marker.
    let Some(tag_close_rel) = html[marker_idx..].find('>') else {
        return String::new();
    };
    let text_start = marker_idx + tag_close_rel + 1;
    // Ends at the next `</a>` or `</div>`.
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

        // Build the search-engine URL from the fixed host + query parameter.
        // An empty/whitespace query is rejected here (fail-closed).
        let url = match build_search_url(&input.query) {
            Ok(url) => url,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("query rejected: {e}"),
                    request.now,
                ));
            }
        };

        // SSRF guarding BEFORE the request (defense in depth).
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

        // NO redirects + timeout. The User-Agent is set because DDG's
        // HTML endpoint may otherwise return an empty body.
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
        // Truncate the body before parsing (memory bound).
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

        // Fetched search-engine content ALWAYS stays untrusted (no .trusted()).
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
            description: "Performs a web search via a public search engine's HTML endpoint \
                 (read-only HTTP GET, keyless; SSRF-guarded: fixed host, http/https only, \
                 no redirects); parses titles/URLs/snippets and stays untrusted."
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
                        "description": "Search term (must not be empty)."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Optional maximum number of results (1..=20)."
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

    /// Small embedded HTML sample that mimics the `DuckDuckGo` HTML
    /// endpoint's result structure (`result__a` anchors + `result__snippet` fragments).
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
        // The query is in the q parameter, spaces encoded (not in the host/path).
        let q = url
            .query_pairs()
            .find(|(k, _)| k == "q")
            .map(|(_, v)| v.into_owned());
        assert_eq!(q, Some("rust async traits".to_string()));
        // The host did NOT change from the user's input.
        assert_eq!(url.host_str(), Some("html.duckduckgo.com"));
    }

    #[test]
    fn build_search_url_trims_and_rejects_empty() {
        assert!(build_search_url("").is_err());
        assert!(build_search_url("   ").is_err());
        assert!(build_search_url("\t\n ").is_err());
        // Leading/trailing whitespace is trimmed but the content is preserved.
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

        // First result: title HTML-stripped, URL from the uddg parameter.
        assert_eq!(results[0].title, "First & Best Result");
        assert_eq!(results[0].url, "https://example.com/first");
        assert!(
            results[0].snippet.contains("first snippet"),
            "snippet parsed: {:?}",
            results[0].snippet
        );

        // Second result: title + decoded URL.
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
        // An invalid %XX is rejected.
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

        // Layer B: the manifest must not contain family names (generic Layer A).
        // Forbidden fragments are built from ROT13, so this test itself does
        // not contain Layer B names as literals.
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
            assert!(!lower.contains(&frag), "Layer B leak in manifest");
        }
    }
}
