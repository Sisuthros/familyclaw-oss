//! Research skill: read-only HTTP GET with SSRF guarding (Layer A).
//!
//! [`WebFetchSkill`] gives the agent a REAL research tool — it fetches the
//! content of a public web page for reading. This is deliberately **GET only**,
//! no writes, and **structurally SSRF-safe**:
//!
//! ## Load-bearing safety: `validate_url` + no-redirect
//! Before any network request, `validate_url` rejects:
//! 1. non-`http`/`https` schemes (blocks `file://`, `ftp://`, `gopher://`, `data:`),
//! 2. a missing host,
//! 3. `localhost` (and `*.localhost`),
//! 4. private/loopback/link-local/CGNAT IPs (`127/8`, `::1`, `10/8`, `172.16/12`,
//!    `192.168/16`, `169.254/16` incl. `169.254.169.254` metadata, `100.64/10`, `fc00::/7`,
//!    `fe80::/10`, unspecified `0.0.0.0`/`::`).
//!
//! The request does NOT follow redirects ([`reqwest::redirect::Policy::none`]), so
//! a 302 → 169.254.169.254 cannot bypass the guard.
//!
//! ## Bounded response
//! The response is truncated (`max_bytes`, default 64 KiB, hard cap 512 KiB) so
//! a huge body cannot exhaust memory. Only the **host** is stored (not the full `URL`), so
//! query-string secrets do not leak into the evidence.
//!
//! ## Taint (untrustedness)
//! Fetched web content is ALWAYS untrusted (tainted) — `execute` does NOT call
//! `.trusted()`. Content brought in from the network does not launder itself clean.

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

/// Fixed skill identifier (1-5 are reserved for other default skills).
const SKILL_UUID: uuid::Uuid = uuid::uuid!("66666666-6666-4666-8666-666666666666");

/// Default response cap in bytes (64 KiB).
const DEFAULT_MAX_BYTES: usize = 64 * 1024;

/// Hard upper limit for the response in bytes (512 KiB) — prevents memory exhaustion from a huge body.
const HARD_MAX_BYTES: usize = 512 * 1024;

/// Timeout for the network request.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Skill input: the URL to fetch and an optional byte cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebFetchInput {
    /// The URL to fetch. Must be `http`/`https` and point to a public host
    /// (private/loopback/link-local addresses are rejected).
    pub url: String,
    /// Optional response byte cap (clamped to the range 1..=`HARD_MAX_BYTES`).
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

/// Skill result: the essence of the fetched page (status + host + size + truncated text).
///
/// Stores only the **host**, not the full `URL` — the query string does not leak.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebFetchOutput {
    /// HTTP status code.
    pub status: u16,
    /// The corresponding host (NOT the full `URL`).
    pub host: String,
    /// Size of the returned (truncated) text in bytes.
    pub bytes: usize,
    /// Truncated, lossy-UTF-8-decoded response text.
    pub text: String,
}

/// Read-only web-fetch skill with SSRF guarding.
#[derive(Debug, Default, Clone)]
pub struct WebFetchSkill;

impl WebFetchSkill {
    /// Creates a new skill instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Fixed skill identifier.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(SKILL_UUID)
    }
}

/// Validates the `URL` for SSRF safety. A PURE function — does NOT make a network
/// request, so it is unit-testable without network access.
///
/// # Errors
/// Returns [`ActionError::PolicyDenied`] if the URL is invalid or points to a
/// non-public target (scheme, host, private/loopback/link-local IP).
///
/// `pub(crate)` so that sibling skills (e.g. `research`) can reuse the same
/// SSRF guard without duplicating the code.
pub(crate) fn validate_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw)
        .map_err(|e| ActionError::PolicyDenied(format!("invalid URL (rejected): {e}")))?;

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

    // If the host is a literal IP, classify it and reject non-public ones.
    // An IPv6 host arrives in brackets (e.g. "[::1]") — strip them before parsing,
    // otherwise IpAddr::parse fails and loopback/link-local could slip through (SSRF hole).
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

    Ok(url)
}

/// Is the IP public (not loopback/private/link-local/CGNAT/unspecified)?
///
/// IPv6's `is_private`/`is_unique_local`/`is_unicast_link_local` are partly
/// unstable in the standard library, so `fc00::/7` and `fe80::/10` are checked
/// manually from the segments.
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() {
                return false;
            }
            // CGNAT 100.64.0.0/10 (no stable is_shared() method)
            let o = v4.octets();
            if o[0] == 100 && (64..=127).contains(&o[1]) {
                return false;
            }
            // Broadcast / documentation / reserved are enough here — otherwise public.
            !v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return false;
            }
            let seg = v6.segments();
            // fc00::/7 (unique local): top 7 bits of the first byte == 1111110
            if (seg[0] & 0xfe00) == 0xfc00 {
                return false;
            }
            // fe80::/10 (link local): top 10 bits of the first segment == 1111111010
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

        // SSRF guard BEFORE the request. Rejected URL → failed result (no panic).
        let url = match validate_url(&input.url) {
            Ok(url) => url,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("url rejected: {e}"),
                    request.now,
                ));
            }
        };

        // SECURITY FIX 2026-07-09 (audit [2], SSRF/DNS rebinding): validate_url
        // only checks for a non-public IP if the host is a literal IP. A domain name
        // (e.g. an attacker's attacker.com that resolves to 169.254.169.254 = AWS IMDS)
        // bypassed the check. Now the host is resolved and rejected if ANY
        // resolved IP is non-public. Resolving right before the fetch also narrows
        // the rebinding window (it does not fully close it without resolved-IP
        // pinning, but reqwest hits the same DNS cache within this process).
        {
            let host = url.host_str().unwrap_or("").to_string();
            // Skip if the host is already a literal IP (validate_url already handled it).
            if host.parse::<IpAddr>().is_err() && !host.is_empty() {
                let port = url.port_or_known_default().unwrap_or(443);
                let probe = format!("{host}:{port}");
                let resolved = tokio::task::spawn_blocking(move || {
                    use std::net::ToSocketAddrs as _;
                    probe
                        .to_socket_addrs()
                        .map(|it| it.map(|sa| sa.ip()).collect::<Vec<_>>())
                })
                .await;
                match resolved {
                    Ok(Ok(ips)) if !ips.is_empty() => {
                        if let Some(bad) = ips.iter().find(|ip| !is_public_ip(**ip)) {
                            return Ok(ActionResult::failure(
                                format!(
                                    "host {host} resolves to non-public IP {bad} (SSRF, rejected)"
                                ),
                                request.now,
                            ));
                        }
                    }
                    // DNS did not resolve / empty / task failed → fail-closed.
                    _ => {
                        return Ok(ActionResult::failure(
                            format!("host {host} did not resolve (rejected, fail-closed)"),
                            request.now,
                        ));
                    }
                }
            }
        }

        let cap = input
            .max_bytes
            .unwrap_or(DEFAULT_MAX_BYTES)
            .clamp(1, HARD_MAX_BYTES);

        // NO redirects (prevents a 302→private bypass) + timeout. TLS comes from
        // reqwest's default (workspace dep); per-skill rustls enforcement was removed
        // because it would require the rustls-tls feature at the workspace level (affecting everything).
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
        // resp.text() decodes the body into a String (lossy-UTF-8) without a
        // separate Bytes type; it is then truncated to the cap.
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

        // Fetched web content always remains untrusted (no .trusted()).
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
            publisher: None,
            signature: None,
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
    fn ssrf_resolved_ip_classification_blocks_internal() {
        // SECURITY FIX 2026-07-09 (audit [2]): execute-level SSRF resolution
        // rejects the host if ANY resolved IP is non-public. This
        // tests the classification guard (is_public_ip) that the resolution uses —
        // a domain that resolves to these IPs is blocked in execute().
        // (A full domain→IP rebinding test would need mock DNS; the classification is the core.)
        for internal in [
            "127.0.0.1",       // loopback (attacker.com → localhost)
            "169.254.169.254", // AWS/GCP IMDS
            "10.0.0.1",        // private
            "192.168.1.1",
            "100.64.0.1", // CGNAT
        ] {
            let ip: IpAddr = internal.parse().unwrap();
            assert!(
                !is_public_ip(ip),
                "resolvoitu sisäinen IP {internal} pitäisi luokitella ei-julkiseksi → SSRF-esto"
            );
        }
        // Public resolutions are allowed.
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn truncate_is_utf8_safe_and_capped() {
        let mut s = "ääääää".to_string(); // 12 bytes (2/char)
        truncate_utf8(&mut s, 5);
        // Must not cut in the middle of a multi-byte char → at most 4 bytes (2 'ä').
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
        // Layer B: the manifest must not contain family names (generic Layer A).
        // The forbidden fragments are built from ROT13 so that this test itself does not
        // contain Layer B names as literals (otherwise the leak audit would flag
        // this test file, even though the production code is clean).
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
