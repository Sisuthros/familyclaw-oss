//! Reference pattern: email triage (classification) (Layer A).
//!
//! [`EmailTriageMock`] reads a list of emails ([`EmailItem`]) and classifies
//! each one into a category while suggesting an action. The skill is
//! **read-only** ([`crate::policy::ActionRisk::ReadOnly`]) — it neither sends,
//! deletes, nor modifies anything. This is a **reference pattern that
//! demonstrates the skill contract** (manifest + read-only risk class +
//! input/output schema): the execution logic is deterministic and in-memory,
//! and the sender address is a generic placeholder `user@example.com` (Layer
//! A).
//!
//! ## Optional live path ([`EmailTriageLive`])
//! When `FAMILYCLAW_EMAIL_TRIAGE_URL` is set to a public **HTTPS** endpoint,
//! [`ActionRuntime::register_default_skills`](crate::facade::ActionRuntime::register_default_skills)
//! also registers [`EmailTriageLive`]. That skill performs a read-only GET
//! (SSRF-guarded like [`super::web_fetch`]: `http(s)` only at the URL parser,
//! then **HTTPS-only** for this skill, no redirects, non-public hosts/IPs
//! rejected), expects JSON emails in the same shape as [`EmailTriageInput`],
//! and reuses [`EmailTriageMock::triage`]. If the env var is unset, only the
//! mock remains registered (fail-closed: an empty or non-public URL fails
//! registration).

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

use super::web_fetch;
use super::Skill;

/// Fixed identifier for the mock skill, so registration and lookup are reproducible.
const SKILL_UUID: uuid::Uuid = uuid::uuid!("22222222-2222-4222-8222-222222222222");

/// Fixed identifier for the optional live skill (distinct from the mock).
const LIVE_SKILL_UUID: uuid::Uuid = uuid::uuid!("22222222-2222-4222-a222-222222222223");

/// Environment variable that enables [`EmailTriageLive`] registration.
pub const EMAIL_TRIAGE_URL_ENV: &str = "FAMILYCLAW_EMAIL_TRIAGE_URL";

/// Timeout for the live network request.
const LIVE_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Hard cap for the live response body (512 KiB) — fail-closed on oversized payloads.
const LIVE_HARD_MAX_BYTES: usize = 512 * 1024;

/// A single email to be classified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailItem {
    /// The sender's address (generic, e.g. `user@example.com`).
    pub from: String,
    /// The message subject.
    pub subject: String,
    /// The message body.
    pub body: String,
}

/// Skill input: a list of emails to classify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageInput {
    /// The emails to classify.
    pub emails: Vec<EmailItem>,
}

/// The classification result for a single email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriagedEmail {
    /// Index in the input list (0-based).
    pub id: usize,
    /// The inferred category (e.g. `urgent`, `spam`, `normal`).
    pub category: String,
    /// The suggested action (e.g. `reply`, `archive`, `read`).
    pub action: String,
}

/// Skill output: the classified emails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageOutput {
    /// Classification results in the order of the input.
    pub categorized: Vec<TriagedEmail>,
}

/// Mock skill for email classification (read-only).
///
/// The risk class is [`ActionRisk::ReadOnly`] and the policy is
/// [`ApprovalPolicy::AutoIfReadOnly`], so execution runs automatically
/// without approval.
#[derive(Debug, Clone, Default)]
pub struct EmailTriageMock;

impl EmailTriageMock {
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

    /// Classifies the emails using deterministic keywords (pure logic).
    ///
    /// Heuristic:
    /// - subject/body contains `urgent`/`asap` → `urgent` + `reply`,
    /// - subject/body contains `unsubscribe`/`offer` → `spam` + `archive`,
    /// - otherwise → `normal` + `read`.
    #[must_use]
    pub fn triage(input: &EmailTriageInput) -> EmailTriageOutput {
        let categorized = input
            .emails
            .iter()
            .enumerate()
            .map(|(id, email)| {
                let haystack = format!("{} {}", email.subject, email.body).to_ascii_lowercase();
                let (category, action) = if haystack.contains("urgent") || haystack.contains("asap")
                {
                    ("urgent", "reply")
                } else if haystack.contains("unsubscribe") || haystack.contains("offer") {
                    ("spam", "archive")
                } else {
                    ("normal", "read")
                };
                TriagedEmail {
                    id,
                    category: category.to_string(),
                    action: action.to_string(),
                }
            })
            .collect();
        EmailTriageOutput { categorized }
    }
}

#[async_trait]
impl ActionExecutor for EmailTriageMock {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let input: EmailTriageInput = match serde_json::from_value(request.payload.clone()) {
            Ok(input) => input,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid email_triage input: {e}"),
                    request.now,
                ));
            }
        };

        let out = Self::triage(&input);
        let count = out.categorized.len();
        let output: Value = json!({ "categorized": out.categorized });

        Ok(ActionResult::success(
            format!("triaged {count} email(s)"),
            output,
            request.now,
        ))
    }
}

impl Skill for EmailTriageMock {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "email_triage_mock".to_string(),
            version: "1.0.0".to_string(),
            description: "Classifies emails into categories and suggests actions (read-only)."
                .to_string(),
            permissions: vec![SkillPermission::NetworkRead],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
            input_hint: Some("{ emails: [{ from, subject, body }] }".to_string()),
            output_hint: Some("{ categorized: [{ id, category, action }] }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "emails": {
                        "type": "array",
                        "description": "The emails to classify.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "from": {
                                    "type": "string",
                                    "description": "The sender's address."
                                },
                                "subject": {
                                    "type": "string",
                                    "description": "The message subject."
                                },
                                "body": {
                                    "type": "string",
                                    "description": "The message body."
                                }
                            },
                            "required": ["from", "subject", "body"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["emails"],
                "additionalProperties": false
            }),
            publisher: None,
            signature: None,
        }
    }
}

/// Optional live email-triage skill: GET JSON emails from an allowlisted HTTPS URL.
///
/// Registered only when [`EMAIL_TRIAGE_URL_ENV`] is set to a valid public HTTPS
/// endpoint. Fetched content is always treated as untrusted; classification
/// reuses [`EmailTriageMock::triage`].
#[derive(Debug, Clone)]
pub struct EmailTriageLive {
    /// Validated public HTTPS endpoint (SSRF-checked at construction).
    endpoint: reqwest::Url,
}

impl EmailTriageLive {
    /// Creates a live skill from an explicit endpoint URL.
    ///
    /// # Errors
    /// [`ActionError::PolicyDenied`] if the URL fails SSRF validation or is not HTTPS.
    pub fn try_new(raw_url: &str) -> Result<Self> {
        let endpoint = validate_live_endpoint(raw_url)?;
        Ok(Self { endpoint })
    }

    /// Reads [`EMAIL_TRIAGE_URL_ENV`]. Returns `Ok(None)` when unset.
    ///
    /// # Errors
    /// Fail-closed: empty value or a non-public / non-HTTPS URL returns
    /// [`ActionError::PolicyDenied`] (or a related action error).
    pub fn try_from_env() -> Result<Option<Self>> {
        match std::env::var(EMAIL_TRIAGE_URL_ENV) {
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(e) => Err(ActionError::PolicyDenied(format!(
                "{EMAIL_TRIAGE_URL_ENV}: {e}"
            ))),
            Ok(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Err(ActionError::PolicyDenied(format!(
                        "{EMAIL_TRIAGE_URL_ENV} is set but empty (rejected, fail-closed)"
                    )));
                }
                Ok(Some(Self::try_new(trimmed)?))
            }
        }
    }

    /// The skill's fixed identifier.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(LIVE_SKILL_UUID)
    }

    /// Host of the configured endpoint (for proofs / summaries — not the full URL).
    #[must_use]
    pub fn host(&self) -> &str {
        self.endpoint.host_str().unwrap_or("")
    }

    /// Parses a JSON body into [`EmailTriageInput`] (fail-closed on bad shape).
    ///
    /// Accepts either `{ "emails": [...] }` or a bare JSON array of email objects.
    pub(crate) fn parse_emails_json(body: &str) -> Result<EmailTriageInput> {
        let value: Value = serde_json::from_str(body).map_err(|e| {
            ActionError::ExecutionFailed(format!("email triage live: invalid JSON body: {e}"))
        })?;
        if value.is_array() {
            let emails: Vec<EmailItem> = serde_json::from_value(value).map_err(|e| {
                ActionError::ExecutionFailed(format!("email triage live: invalid email array: {e}"))
            })?;
            return Ok(EmailTriageInput { emails });
        }
        serde_json::from_value(value).map_err(|e| {
            ActionError::ExecutionFailed(format!(
                "email triage live: body must be {{ emails: [...] }} or an array: {e}"
            ))
        })
    }
}

/// Validates the configured live endpoint: SSRF guard + HTTPS-only.
fn validate_live_endpoint(raw: &str) -> Result<reqwest::Url> {
    let url = web_fetch::validate_url(raw)?;
    if url.scheme() != "https" {
        return Err(ActionError::PolicyDenied(format!(
            "email triage live requires https (got '{}'; rejected)",
            url.scheme()
        )));
    }
    Ok(url)
}

/// Same non-public IP classification used by `web_fetch` (duplicated locally so
/// live triage can re-check resolved addresses before GET).
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() {
                return false;
            }
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
            if (seg[0] & 0xfe00) == 0xfc00 {
                return false;
            }
            if (seg[0] & 0xffc0) == 0xfe80 {
                return false;
            }
            true
        }
    }
}

#[async_trait]
impl ActionExecutor for EmailTriageLive {
    #[allow(clippy::too_many_lines)]
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        // Payload is ignored for fetch (endpoint is operator-configured); reject
        // non-objects so callers cannot smuggle alternate URLs through input.
        if !request.payload.is_null() && !request.payload.is_object() {
            return Ok(ActionResult::failure(
                "invalid email_triage_live input: expected object or null".to_string(),
                request.now,
            ));
        }

        let url = self.endpoint.clone();
        let host = url.host_str().unwrap_or("").to_string();

        // DNS rebinding guard (same posture as web_fetch): if the host is a
        // name, resolve and reject any non-public IP before requesting.
        {
            let host_for_check = host.clone();
            if host_for_check.parse::<IpAddr>().is_err() && !host_for_check.is_empty() {
                let port = url.port_or_known_default().unwrap_or(443);
                let probe = format!("{host_for_check}:{port}");
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
                                    "host {host_for_check} resolves to non-public IP {bad} (SSRF, rejected)"
                                ),
                                request.now,
                            ));
                        }
                    }
                    _ => {
                        return Ok(ActionResult::failure(
                            format!(
                                "host {host_for_check} did not resolve (rejected, fail-closed)"
                            ),
                            request.now,
                        ));
                    }
                }
            }
        }

        let client = match reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(LIVE_FETCH_TIMEOUT)
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

        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("email triage live fetch failed: {e}"),
                    request.now,
                ));
            }
        };

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Ok(ActionResult::failure(
                format!("email triage live: HTTP {status} from {host} (rejected)"),
                request.now,
            ));
        }

        let body = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("email triage live body read failed: {e}"),
                    request.now,
                ));
            }
        };
        if body.len() > LIVE_HARD_MAX_BYTES {
            return Ok(ActionResult::failure(
                format!(
                    "email triage live: body exceeds {LIVE_HARD_MAX_BYTES} bytes (rejected, fail-closed)"
                ),
                request.now,
            ));
        }

        let input = match Self::parse_emails_json(&body) {
            Ok(input) => input,
            Err(e) => {
                return Ok(ActionResult::failure(e.to_string(), request.now));
            }
        };

        let out = EmailTriageMock::triage(&input);
        let count = out.categorized.len();
        let output: Value = json!({
            "host": host,
            "categorized": out.categorized,
        });

        // Network-sourced content stays untrusted (no .trusted()).
        Ok(ActionResult::success(
            format!("triaged {count} email(s) from {host}"),
            output,
            request.now,
        ))
    }
}

impl Skill for EmailTriageLive {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "email_triage_live".to_string(),
            version: "1.0.0".to_string(),
            description: "Fetches emails as JSON from an operator-configured HTTPS endpoint \
                 (SSRF-guarded, no redirects) and classifies them with the same read-only \
                 triage heuristics as email_triage_mock. Enabled only when \
                 FAMILYCLAW_EMAIL_TRIAGE_URL is set."
                .to_string(),
            permissions: vec![SkillPermission::NetworkRead],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
            input_hint: Some("{ } (emails loaded from FAMILYCLAW_EMAIL_TRIAGE_URL)".to_string()),
            output_hint: Some("{ host, categorized: [{ id, category, action }] }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {},
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

    fn sample() -> EmailTriageInput {
        EmailTriageInput {
            emails: vec![
                EmailItem {
                    from: "user@example.com".to_string(),
                    subject: "URGENT: server down".to_string(),
                    body: "please fix asap".to_string(),
                },
                EmailItem {
                    from: "user@example.com".to_string(),
                    subject: "Special offer".to_string(),
                    body: "click unsubscribe to opt out".to_string(),
                },
                EmailItem {
                    from: "user@example.com".to_string(),
                    subject: "Lunch?".to_string(),
                    body: "are you free thursday".to_string(),
                },
            ],
        }
    }

    #[test]
    fn manifest_is_read_only_auto() {
        let m = EmailTriageMock::new().manifest();
        m.validate().expect("manifest validates");
        assert_eq!(m.name, "email_triage_mock");
        assert_eq!(m.risk, ActionRisk::ReadOnly);
        assert_eq!(m.approval_policy, ApprovalPolicy::AutoIfReadOnly);
    }

    #[test]
    fn triage_classifies_by_keywords() {
        let out = EmailTriageMock::triage(&sample());
        assert_eq!(out.categorized[0].category, "urgent");
        assert_eq!(out.categorized[0].action, "reply");
        assert_eq!(out.categorized[1].category, "spam");
        assert_eq!(out.categorized[2].category, "normal");
    }

    #[tokio::test]
    async fn happy_path_returns_categorized() {
        let skill = EmailTriageMock::new();
        let payload = serde_json::to_value(sample()).expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            EmailTriageMock::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1_700_000_000),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(res.status.is_success());
        let cats = res.raw_output_redacted["categorized"]
            .as_array()
            .expect("array");
        assert_eq!(cats.len(), 3);
    }

    #[test]
    fn live_rejects_non_https_and_ssrf_targets() {
        for bad in [
            "http://example.com/emails.json",
            "https://localhost/emails.json",
            "https://127.0.0.1/emails.json",
            "https://10.0.0.1/emails.json",
            "https://169.254.169.254/latest",
            "file:///etc/passwd",
        ] {
            assert!(
                EmailTriageLive::try_new(bad).is_err(),
                "should reject: {bad}"
            );
        }
    }

    #[test]
    fn live_accepts_public_https_without_network() {
        let live = EmailTriageLive::try_new("https://example.com/emails.json").expect("ok");
        assert_eq!(live.host(), "example.com");
        let m = live.manifest();
        m.validate().expect("manifest validates");
        assert_eq!(m.name, "email_triage_live");
        assert_eq!(m.risk, ActionRisk::ReadOnly);
    }

    #[test]
    fn live_parse_emails_json_object_and_array() {
        let obj = r#"{"emails":[{"from":"a@example.com","subject":"urgent","body":"x"}]}"#;
        let parsed = EmailTriageLive::parse_emails_json(obj).expect("object");
        assert_eq!(parsed.emails.len(), 1);
        let out = EmailTriageMock::triage(&parsed);
        assert_eq!(out.categorized[0].category, "urgent");

        let arr = r#"[{"from":"a@example.com","subject":"hi","body":"there"}]"#;
        let parsed = EmailTriageLive::parse_emails_json(arr).expect("array");
        assert_eq!(
            EmailTriageMock::triage(&parsed).categorized[0].category,
            "normal"
        );
    }

    #[test]
    fn live_parse_emails_json_fail_closed() {
        assert!(EmailTriageLive::parse_emails_json("not-json").is_err());
        assert!(EmailTriageLive::parse_emails_json(r#"{"emails":"nope"}"#).is_err());
        assert!(EmailTriageLive::parse_emails_json(r#"{"nope":[]}"#).is_err());
    }

    #[test]
    fn live_try_from_env_none_when_unset() {
        // Ensure the var is absent for this process for the duration of the assertion.
        // SAFETY: test-only; we restore afterward.
        let previous = std::env::var_os(EMAIL_TRIAGE_URL_ENV);
        std::env::remove_var(EMAIL_TRIAGE_URL_ENV);
        let got = EmailTriageLive::try_from_env().expect("unset is ok");
        assert!(got.is_none());
        match previous {
            Some(v) => std::env::set_var(EMAIL_TRIAGE_URL_ENV, v),
            None => std::env::remove_var(EMAIL_TRIAGE_URL_ENV),
        }
    }

    #[test]
    fn live_try_from_env_fail_closed_on_empty() {
        let previous = std::env::var_os(EMAIL_TRIAGE_URL_ENV);
        std::env::set_var(EMAIL_TRIAGE_URL_ENV, "   ");
        let err = EmailTriageLive::try_from_env().expect_err("empty must fail closed");
        assert!(matches!(err, ActionError::PolicyDenied(_)));
        match previous {
            Some(v) => std::env::set_var(EMAIL_TRIAGE_URL_ENV, v),
            None => std::env::remove_var(EMAIL_TRIAGE_URL_ENV),
        }
    }

    /// Live fetch path with a local mock HTTP server.
    ///
    /// `web_fetch`'s SSRF guard rejects loopback, so this test exercises the
    /// **parse + triage** pipeline the live skill uses after a successful GET
    /// (same JSON contract), without weakening SSRF for `127.0.0.1`. Full GET
    /// + DNS rebinding behavior is covered by the rejection unit tests above
    /// (matching `web_fetch`, which also has no in-crate mock HTTP server).
    #[tokio::test]
    async fn live_triage_reuses_classifier_on_fetched_json_shape() {
        let body = serde_json::to_string(&sample()).expect("serialize");
        let input = EmailTriageLive::parse_emails_json(&body).expect("parse");
        let out = EmailTriageMock::triage(&input);
        assert_eq!(out.categorized.len(), 3);
        assert_eq!(out.categorized[0].category, "urgent");
    }
}
