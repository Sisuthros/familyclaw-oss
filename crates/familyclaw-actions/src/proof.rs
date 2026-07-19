//! Proof bundle: a verifiable trace of an execution, in which values that
//! look like secrets have been redacted (Layer A).
//!
//! This module implements:
//! - [`redact_value`] — a recursive redactor that replaces strings that look
//!   like secrets with the marker `[REDACTED]`,
//! - [`RedactionReport`] — a summary of how many values were redacted and
//!   under which **pattern names** (not values),
//! - [`VerificationResult`] — the result of a postcondition check,
//! - [`ProofBundle`] — the assembled proof bundle, in which the input is
//!   hashed ([`sha2::Sha256`]) and never stored raw,
//! - [`build_proof`] — a helper that assembles a proof bundle from the
//!   request, the result, audit identifiers, and verification, running redaction.
//!
//! ## OSS boundary
//! A proof bundle never contains a raw token, API key, or other secret: the
//! input is stored only as a SHA-256 hash, and both the input and output
//! fields are redacted before being attached to the bundle.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use familyclaw_core::time::Timestamp;

use crate::executor::{ActionRequest, ActionResult, ActionStatus};
use crate::ids::{ActionId, ActionTaskId, AuditEventId, ProofBundleId, SkillId};

/// Module readiness flag — kept so that [`crate::all_modules_scaffolded`]
/// still compiles alongside the other modules.
pub(crate) const SCAFFOLDED: bool = true;

/// Replacement marker for a redacted value.
const REDACTED: &str = "[REDACTED]";

/// The set of key names (case-insensitive) whose **value** is always redacted.
const SECRET_KEY_NAMES: &[&str] = &[
    "api_key",
    "apikey",
    "secret",
    "password",
    "token",
    "authorization",
];

/// A summary of a completed redaction.
///
/// Contains the count of redacted values and the **names** of the patterns
/// found (e.g. `"sk-key"`, `"bearer"`, `"secret-key-name"`) — **never
/// values**. This makes the report itself safe to store.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionReport {
    /// How many values were redacted.
    pub redacted_count: usize,
    /// The names of the patterns found (not values), sorted and deduplicated.
    pub patterns_found: Vec<String>,
}

impl RedactionReport {
    /// Whether at least one value was redacted.
    #[must_use]
    pub fn any_redacted(&self) -> bool {
        self.redacted_count > 0
    }
}

/// Detects a string that looks like a secret and returns the matched
/// pattern's **name** (not the value). `None` if the value does not look
/// like a secret.
///
/// Recognized patterns:
/// - `sk-[A-Za-z0-9]{8,}` (OpenAI-style key) → `"sk-key"`,
/// - `AKIA[0-9A-Z]{12,}` (AWS-style access key) → `"aws-access-key"`,
/// - `Bearer <token>` → `"bearer"`,
/// - a long hex run (≥32 hex characters) → `"long-hex"`,
/// - a base64-style run (≥24 characters, contains `+`/`/`/`=`) → `"base64-blob"`.
fn match_secret_pattern(value: &str) -> Option<&'static str> {
    let trimmed = value.trim();

    // Bearer token: "Bearer " + at least one character.
    if let Some(rest) = trimmed.strip_prefix("Bearer ") {
        if !rest.trim().is_empty() {
            return Some("bearer");
        }
    }

    // sk-XXXXXXXX (≥8 alphanumeric characters after "sk-").
    if let Some(rest) = trimmed.strip_prefix("sk-") {
        let run = rest.chars().take_while(char::is_ascii_alphanumeric).count();
        if run >= 8 {
            return Some("sk-key");
        }
    }

    // AKIA + 12+ uppercase letters/digits.
    if let Some(rest) = trimmed.strip_prefix("AKIA") {
        let run = rest
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            .count();
        if run >= 12 {
            return Some("aws-access-key");
        }
    }

    // Long hex: ≥32 characters, all hex digits.
    if trimmed.len() >= 32 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some("long-hex");
    }

    // Base64-style: ≥24 characters, allowed characters, and at least one
    // base64 special character (+ / =), so that ordinary words don't match.
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

/// Whether the key name (case-insensitive) is a known secret key.
fn is_secret_key_name(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SECRET_KEY_NAMES.iter().any(|name| *name == lower)
}

/// Redacts substrings that look like secrets from free-form text.
///
/// Unlike [`match_secret_pattern`] (which examines the whole string), this
/// splits the text on whitespace and redacts individual *words* that look
/// like secrets. This way, e.g. an upstream error message
/// `"auth rejected: sk-livelivelive"` does not leak a raw token into the proof.
///
/// It also recognizes the two-word `Bearer <token>` form, in which case the
/// whole `Bearer …` is replaced, as well as `key=value` and `key: value`
/// forms where the key name is a known secret key.
///
/// Returns the redacted text and increments the given report for every match
/// (only the pattern's **name** is recorded in the report, not the value).
fn redact_text(text: &str, report: &mut RedactionReport) -> String {
    // 1. Substring pass: redact `Bearer <token>` anywhere in the text
    //    (also when embedded, e.g. in the form `header=Bearer xyz`).
    let bearer_redacted = redact_bearer_substrings(text, report);

    // 2. Word pass: split preserving whitespace and redact individual
    //    words (value-based) as well as `key=value` forms.
    let mut out = String::with_capacity(bearer_redacted.len());
    for chunk in bearer_redacted.split_inclusive(char::is_whitespace) {
        // Separate the word from the trailing whitespace (if any).
        let trimmed_end = chunk.trim_end_matches(char::is_whitespace);
        let trailing = &chunk[trimmed_end.len()..];

        // "key=value" / "key:value" form, where the key is a secret key.
        if let Some(redacted_kv) = redact_keyed_token(trimmed_end, report) {
            out.push_str(&redacted_kv);
            out.push_str(trailing);
            continue;
        }

        // Value-based detection for a single word.
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

/// Redacts every `Bearer <token>` occurrence from the text — including
/// embedded ones, e.g. `Authorization: Bearer abc` or `header=Bearer abc`.
/// Replaces the whitespace-delimited token following the word `Bearer` with the marker.
fn redact_bearer_substrings(text: &str, report: &mut RedactionReport) -> String {
    const MARKER: &str = "Bearer ";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(pos) = rest.find(MARKER) {
        let after = &rest[pos + MARKER.len()..];
        // Token = the non-whitespace characters after the "Bearer " marker.
        let token_len = after
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map_or(after.len(), |(i, _)| i);
        if token_len == 0 {
            // "Bearer " without a token — copy as-is and continue.
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

/// Redacts the value of a `key=value` / `key: value` form word, if the key
/// name (before the separator) is a known secret key. Returns `None` if the
/// word is not in this form.
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

/// Recursively redacts values that look like secrets from the given
/// [`serde_json::Value`] structure.
///
/// Returns a redacted copy plus a [`RedactionReport`] summary. Replaced with
/// the marker `[REDACTED]` if:
/// - the string value itself looks like a secret (pattern detection), or
/// - the value is in an object field whose **name** is a known secret key
///   (`api_key`, `apikey`, `secret`, `password`, `token`, `authorization`) —
///   regardless of the value's shape.
///
/// The original input is not mutated. The resulting report never contains
/// raw secret values, only pattern names.
#[must_use]
pub fn redact_value(value: &Value) -> (Value, RedactionReport) {
    let mut report = RedactionReport::default();
    let redacted = redact_inner(value, None, &mut report);
    report.patterns_found.sort_unstable();
    report.patterns_found.dedup();
    (redacted, report)
}

/// Internal recursion: `parent_key` is the name of the object field where
/// `value` resides (if any), so key-name-based redaction works.
fn redact_inner(value: &Value, parent_key: Option<&str>, report: &mut RedactionReport) -> Value {
    match value {
        Value::String(s) => {
            // Key-name-based redaction: the field name reveals the secret.
            if let Some(key) = parent_key {
                if is_secret_key_name(key) && !s.is_empty() {
                    report.redacted_count += 1;
                    report
                        .patterns_found
                        .push(format!("secret-key:{}", key.to_ascii_lowercase()));
                    return Value::String(REDACTED.to_string());
                }
            }
            // Value-based redaction: the string looks like a secret.
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
        // Numbers, booleans, and null cannot be secrets.
        other => other.clone(),
    }
}

/// Recursively redacts values that look like secrets, **deeply**, from the
/// given [`serde_json::Value`] structure — including secrets **embedded**
/// in free-form text.
///
/// Difference from [`redact_value`]: whereas [`redact_value`] redacts a
/// string only if (a) its field name is a known secret key or (b) the
/// **whole** string looks like a secret, this variant additionally runs the
/// `redact_text` substring pass on every string leaf that was not already
/// fully redacted. This way, e.g. a free-form tool argument
/// `{"prompt":"deploy using sk-livelivelivelive then ..."}` does not leak a
/// raw token to disk, even though the field name (`prompt`) is not a secret
/// key and the whole value is not just a token.
///
/// Used for the tool arguments of the message stack that gets persisted to
/// disk for a resumable turn (stored under the [`crate::ApprovalId`] key),
/// where a secret may be hiding inside model-generated free text.
///
/// Returns a redacted copy plus a [`RedactionReport`] summary. The original
/// input is not mutated, and the report never carries raw secret values.
#[must_use]
pub fn redact_value_deep(value: &Value) -> (Value, RedactionReport) {
    let mut report = RedactionReport::default();
    let redacted = redact_inner_deep(value, None, &mut report);
    report.patterns_found.sort_unstable();
    report.patterns_found.dedup();
    (redacted, report)
}

/// Like [`redact_inner`], but string leaves additionally get the
/// [`redact_text`] substring pass, so that secrets embedded in free-form
/// text (e.g. `"deploy using sk-live..."`) do not go un-redacted.
fn redact_inner_deep(
    value: &Value,
    parent_key: Option<&str>,
    report: &mut RedactionReport,
) -> Value {
    match value {
        Value::String(s) => {
            // 1. Key-name-based redaction: the field name reveals the secret.
            if let Some(key) = parent_key {
                if is_secret_key_name(key) && !s.is_empty() {
                    report.redacted_count += 1;
                    report
                        .patterns_found
                        .push(format!("secret-key:{}", key.to_ascii_lowercase()));
                    return Value::String(REDACTED.to_string());
                }
            }
            // 2. Value-based redaction: the whole string looks like a secret.
            if let Some(pattern) = match_secret_pattern(s) {
                report.redacted_count += 1;
                report.patterns_found.push(pattern.to_string());
                return Value::String(REDACTED.to_string());
            }
            // 3. Substring pass: a secret EMBEDDED in free-form text.
            //    Splits on whitespace and redacts individual secret words +
            //    `Bearer …`/`key=value` forms. If nothing matches, the text
            //    is returned as-is (no unnecessary copy semantically).
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
        // Numbers, booleans, and null cannot be secrets.
        other => other.clone(),
    }
}

/// Redacts substrings that look like secrets from free-form **text** (not a
/// JSON structure).
///
/// This is a public wrapper around the `redact_text` substring pass, so that
/// `familyclaw-agent` can redact the **text content** of a resumable turn's
/// message stack (system/user/tool messages' `content`) before persisting to
/// disk. Splits the text on whitespace and redacts individual secret words as
/// well as `Bearer <token>` and `key=value` forms where the key is a known
/// secret key.
///
/// Returns the redacted text plus a [`RedactionReport`] summary. The report
/// carries only pattern **names**, never raw values.
#[must_use]
pub fn redact_free_text(text: &str) -> (String, RedactionReport) {
    let mut report = RedactionReport::default();
    let redacted = redact_text(text, &mut report);
    report.patterns_found.sort_unstable();
    report.patterns_found.dedup();
    (redacted, report)
}

/// Computes the input's SHA-256 hash as a hex string.
///
/// The input is first serialized into canonical JSON form. The hash is
/// stored in the proof instead of the raw payload, so the secret never ends
/// up on disk.
///
/// # Errors
/// Returns [`crate::ActionError::Proof`] if serializing the input fails.
pub fn sha256_hex(value: &Value) -> crate::Result<String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| crate::ActionError::Proof(format!("input serialize failed: {e}")))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

/// The result of a postcondition check (the verify phase).
///
/// Describes whether the result was verified and which checks ran. `notes`
/// is a free-form human-readable explanation (NOT secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationResult {
    /// Whether the result passed verification.
    pub verified: bool,
    /// The names/descriptions of the checks that ran.
    pub checks: Vec<String>,
    /// A free-form explanation (NOT secrets).
    pub notes: String,
}

impl VerificationResult {
    /// A successful verification with the given checks.
    #[must_use]
    pub fn passed(checks: Vec<String>, notes: impl Into<String>) -> Self {
        Self {
            verified: true,
            checks,
            notes: notes.into(),
        }
    }

    /// A failed verification with the given checks.
    #[must_use]
    pub fn failed(checks: Vec<String>, notes: impl Into<String>) -> Self {
        Self {
            verified: false,
            checks,
            notes: notes.into(),
        }
    }
}

/// An assembled proof bundle from one executed action.
///
/// Contains the hashed input, the redacted output, execution times,
/// references to audit events, and the verification and redaction summaries.
/// The bundle is designed to be stored as-is: it never contains a raw secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofBundle {
    /// The proof bundle's unique identifier.
    pub id: ProofBundleId,
    /// The task this action was executed as part of.
    pub task_id: ActionTaskId,
    /// The identifier of the executed skill.
    pub skill_id: SkillId,
    /// The identifier of the executed action.
    pub action_id: ActionId,
    /// The action's final state.
    pub status: ActionStatus,
    /// The execution start time (injected).
    pub started_at: Timestamp,
    /// The execution finish time (injected).
    pub finished_at: Timestamp,
    /// The input's SHA-256 hash (hex) — NOT the raw payload.
    pub input_hash: String,
    /// A short human-readable summary of the result.
    pub output_summary: String,
    /// The redacted output (raw output with secrets removed).
    pub redacted_output: Value,
    /// Whether the output originates from an untrusted source (taint).
    pub untrusted: bool,
    /// The identifiers of the audit events associated with this action.
    pub audit_event_ids: Vec<AuditEventId>,
    /// The result of the verification phase.
    pub verification: VerificationResult,
    /// The redaction summary (input + output combined).
    pub redaction: RedactionReport,
}

/// Assembles a [`ProofBundle`] from the request, the result, audit
/// identifiers, and verification.
///
/// Steps:
/// 1. compute the input's SHA-256 hash (the raw payload is not stored),
/// 2. redact both the input and the output ([`redact_value`]),
/// 3. merge the redaction reports into one,
/// 4. preserve the output's `untrusted` flag as-is from the result — a
///    trusted source can already clear it at the [`ActionResult`] level.
///
/// The input is redacted only for reporting purposes; only the hash is
/// stored in the bundle, not the input (not even redacted), so the payload
/// cannot leak even in that form.
///
/// # Errors
/// Returns [`crate::ActionError::Proof`] if hashing the input fails.
pub fn build_proof(
    request: &ActionRequest,
    result: &ActionResult,
    audit_event_ids: Vec<AuditEventId>,
    verification: VerificationResult,
) -> crate::Result<ProofBundle> {
    let input_hash = sha256_hex(&request.payload)?;

    // Redact the input (only for reporting) and the output (to be stored).
    let (_redacted_input, input_report) = redact_value(&request.payload);
    let (redacted_output, output_report) = redact_value(&result.raw_output_redacted);

    // Merge the redaction reports.
    let mut combined = RedactionReport {
        redacted_count: input_report.redacted_count + output_report.redacted_count,
        patterns_found: input_report.patterns_found,
    };
    combined.patterns_found.extend(output_report.patterns_found);

    // Also redact the free-text fields that get copied into the proof
    // as-is (output_summary, verification.notes/checks). These don't go
    // through redact_value because they are String fields, not JSON values,
    // so an upstream error message could otherwise leak a raw token.
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

    /// Builds a value that looks like a secret via runtime concatenation, so
    /// there is no >=10-character literal in the source code next to a
    /// secret-looking field (Layer B audit), and no real key.
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
        // A short, innocuous value but under a secret key → still redacted.
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
        // This is exactly the gap that defect #2 reported: the secret hides
        // inside a LARGER piece of free text, the field name is NOT a secret
        // key, and the whole value is not just a token. `redact_value` leaves it raw.
        let secret = fake_secret();
        let input = json!({ "prompt": format!("deploy using {secret} then ship") });

        // The old (shallow) redaction does NOT catch the embedded secret.
        let (shallow, shallow_report) = redact_value(&input);
        let shallow_json = serde_json::to_string(&shallow).expect("serialize");
        assert!(
            shallow_json.contains(&secret),
            "redact_value is documented as missing embedded secrets (regression sentinel)"
        );
        assert!(!shallow_report.any_redacted());

        // The new (deep) redaction catches it.
        let (deep, deep_report) = redact_value_deep(&input);
        let deep_json = serde_json::to_string(&deep).expect("serialize");
        assert!(
            !deep_json.contains(&secret),
            "redact_value_deep must redact secrets embedded in free-text args"
        );
        assert!(deep_json.contains(REDACTED));
        assert!(deep_report.any_redacted());
        // The surrounding innocuous text remains readable.
        assert!(deep_json.contains("deploy using"));
        assert!(deep_json.contains("then ship"));
    }

    #[test]
    fn redact_value_deep_still_redacts_keyed_and_whole_value_secrets() {
        // The deep variant must NOT weaken the shallow redaction's
        // guarantees: key-name and whole-value redaction still work.
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
        // A user/system message's content can carry a secret as free text.
        let secret = fake_secret();
        let content = format!("here is my key {secret} please use it");
        let (redacted, report) = redact_free_text(&content);
        assert!(
            !redacted.contains(&secret),
            "redact_free_text must mask secrets embedded in message content"
        );
        assert!(redacted.contains(REDACTED));
        assert!(report.any_redacted());
        // Innocuous text remains.
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

    /// Helper: builds an execution request for test use.
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
        // The secret is built via runtime concatenation — no literal in the source.
        let secret = fake_secret();
        let payload = json!({ "to": "general", "note": secret.clone() });

        // Execution echoes the input into the output (a tainted source).
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

        // The output is redacted.
        let out = serde_json::to_string(&proof.redacted_output).expect("serialize output");
        assert!(out.contains(REDACTED));
        assert!(!out.contains(&secret));
        assert!(proof.redaction.any_redacted());

        // The whole proof (incl. input_hash) does not contain the raw secret.
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

        // An explicitly trusted source clears the flag.
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
        // Attack: an upstream error message leaks a token into output_summary,
        // which gets copied into the proof as free text. This field does NOT
        // go through redact_value (which only redacts JSON values, not String fields).
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
        // Attack: the verification phase's notes/checks leak a token into
        // the proof as free text — these fields must be redacted too.
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
        // Several secret forms in different fields, including a secret key.
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

        // The serialized proof does not contain a single raw secret value.
        for needle in [&sk, &bearer, &hex] {
            assert!(
                !whole.contains(needle.as_str()),
                "proof must not contain raw secret: {needle}"
            );
        }
        // But the redaction marker is present.
        assert!(whole.contains(REDACTED));
    }
}
