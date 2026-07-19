//! Policy layer: an action's risk class, required capabilities
//! (permissions), and approval policy (Layer A, generic — no real
//! providers, personas, or keys).
//!
//! This module defines:
//! - [`SkillPermission`] — a single capability a skill needs,
//! - [`ActionRisk`] — an action's risk class,
//! - [`ApprovalPolicy`] — when human approval is required,
//! - [`detect_secret_like`] — a heuristic detector for strings that
//!   resemble a secret (used in manifest validation and
//!   proof redaction).
//!
//! Determinism: this module does not read the clock or make network calls.

use serde::{Deserialize, Serialize};

/// Module readiness level (scaffold compatibility).
///
/// Kept so that [`crate::all_modules_scaffolded`] still compiles alongside
/// other modules that are still in the scaffold stage.
pub(crate) const SCAFFOLDED: bool = true;

/// A single capability (permission) that a skill needs in order to operate.
///
/// Generic — no references to real providers or services.
/// The policy ([`ApprovalPolicy`]) and risk class ([`ActionRisk`]) are derived
/// in part from these permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillPermission {
    /// Read local files.
    ReadFiles,
    /// Write local files (reversible).
    WriteLocalFiles,
    /// Read data from the network (read-only, no side effects).
    NetworkRead,
    /// Send a message (e.g. to a chat channel) — has side effects.
    SendMessage,
    /// Execute code.
    ExecuteCode,
    /// Spend money (a payment transaction).
    SpendMoney,
    /// Write to an external system (an unrecoverable side effect).
    WriteExternal,
}

/// An action's risk class, intended in increasing order of danger.
///
/// The class drives the default approval behavior
/// ([`ApprovalPolicy::requires_human`]) and the weight given to audit logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionRisk {
    /// Read-only — no side effects.
    ReadOnly,
    /// Local write (reversible).
    WriteLocal,
    /// External write (affects a third-party system).
    WriteExternal,
    /// An irreversible action.
    Irreversible,
    /// Spending money.
    SpendMoney,
    /// Sending a message (externally visible).
    SendMessage,
    /// Code execution.
    ExecuteCode,
}

impl ActionRisk {
    /// Whether this risk class is read-only (no side effects).
    #[must_use]
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }

    /// Whether this risk class can *ever* produce an automatic run
    /// ([`ApprovalRequirement::AutoRun`]) under some policy.
    ///
    /// Only [`ActionRisk::ReadOnly`] and [`ActionRisk::WriteLocal`] can run
    /// automatically (read always, local write under the `RequireApproval`
    /// policy). Higher-risk classes (money, irreversible, external
    /// write, message, code) always require approval regardless of
    /// policy — they are **not** auto-runnable.
    ///
    /// Used in manifest cross-validation: a high-risk permission must not
    /// be tagged with an auto-runnable risk class.
    #[must_use]
    pub const fn is_auto_runnable_class(self) -> bool {
        matches!(self, Self::ReadOnly | Self::WriteLocal)
    }
}

impl SkillPermission {
    /// Whether this permission requires that the declared risk class NOT be
    /// auto-runnable (i.e. the permission is always a side effect requiring
    /// approval).
    ///
    /// Manifest cross-validation ([`crate::manifest::SkillManifest::validate`])
    /// uses this to prevent a situation where a money-spending
    /// ([`SkillPermission::SpendMoney`]) or externally-writing
    /// ([`SkillPermission::WriteExternal`]) skill is tagged as e.g.
    /// [`ActionRisk::ReadOnly`] risk and thereby bypasses approval in the
    /// pipeline.
    #[must_use]
    pub const fn forbids_auto_run_risk(self) -> bool {
        matches!(self, Self::SpendMoney | Self::WriteExternal)
    }

    /// Whether this permission requires that the declared risk class be
    /// exactly [`ActionRisk::SpendMoney`].
    ///
    /// Spending money ([`SkillPermission::SpendMoney`]) must not masquerade
    /// as a lower risk: if the permission is present, the risk class must be
    /// [`ActionRisk::SpendMoney`], so that audit and approval treat it as
    /// spending money.
    #[must_use]
    pub const fn requires_spend_money_risk(self) -> bool {
        matches!(self, Self::SpendMoney)
    }
}

/// Approval policy: when human approval is required before execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    /// Approval is skipped only if the action is read-only
    /// ([`ActionRisk::ReadOnly`]); otherwise approval is required.
    AutoIfReadOnly,
    /// Approval is required unless the risk class is read-only.
    RequireApproval,
    /// Approval is always required, regardless of risk class.
    AlwaysRequireApproval,
}

/// The approval requirement returned by [`required_approval`]: whether an
/// action may run automatically or human approval is required first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRequirement {
    /// The action may run without separate approval.
    AutoRun,
    /// The action requires human approval before execution.
    RequireApproval,
}

impl ApprovalRequirement {
    /// Whether the requirement mandates human approval.
    #[must_use]
    pub const fn requires_approval(self) -> bool {
        matches!(self, Self::RequireApproval)
    }
}

/// Resolves the approval requirement based on the risk class and policy.
///
/// Rule logic (fail-safe — approval is required in uncertain situations):
/// - [`ActionRisk::SpendMoney`] and [`ActionRisk::Irreversible`] **always**
///   require approval, even if the policy tries to bypass it
///   (e.g. the manifest requests auto-run). These never run on their own.
/// - [`ActionRisk::WriteExternal`], [`ActionRisk::SendMessage`] and
///   [`ActionRisk::ExecuteCode`] require approval by default.
/// - [`ActionRisk::ReadOnly`] and [`ActionRisk::WriteLocal`] may run
///   automatically **unless** the policy forces approval
///   ([`ApprovalPolicy::AlwaysRequireApproval`], or any policy that does
///   not permit auto-run for a non-read-only class).
///
/// Role of the policy: [`ApprovalPolicy::AutoIfReadOnly`] permits auto-run
/// only for the read-only class; [`ApprovalPolicy::RequireApproval`] permits
/// it for the read-only and local-write classes;
/// [`ApprovalPolicy::AlwaysRequireApproval`] never permits it. High-risk
/// classes (money, irreversible, external, message, code) never run
/// automatically regardless of policy.
#[must_use]
pub const fn required_approval(risk: ActionRisk, policy: ApprovalPolicy) -> ApprovalRequirement {
    use ActionRisk::{ExecuteCode, Irreversible, ReadOnly, SendMessage, SpendMoney, WriteLocal};

    // Fail-safe: money + irreversible always require approval.
    if matches!(risk, SpendMoney | Irreversible) {
        return ApprovalRequirement::RequireApproval;
    }

    // Externally visible classes with side effects require approval by
    // default (external write, message, code execution).
    if matches!(risk, ExecuteCode | SendMessage) {
        return ApprovalRequirement::RequireApproval;
    }

    // A policy that always forces approval → require approval.
    if matches!(policy, ApprovalPolicy::AlwaysRequireApproval) {
        return ApprovalRequirement::RequireApproval;
    }

    // The rest (ReadOnly, WriteLocal, WriteExternal) follow the policy.
    match (risk, policy) {
        // Read-only may run automatically under both non-forcing policies.
        (ReadOnly, ApprovalPolicy::AutoIfReadOnly | ApprovalPolicy::RequireApproval) => {
            ApprovalRequirement::AutoRun
        }
        // Local write may run only under the RequireApproval policy
        // (which permits the less side-effect-prone local write),
        // NOT under AutoIfReadOnly which permits only pure reads.
        (WriteLocal, ApprovalPolicy::RequireApproval) => ApprovalRequirement::AutoRun,
        // External write and all other combinations → require approval.
        _ => ApprovalRequirement::RequireApproval,
    }
}

impl ApprovalPolicy {
    /// Whether the given risk class requires human approval.
    ///
    /// - [`ApprovalPolicy::AlwaysRequireApproval`] always requires it.
    /// - [`ApprovalPolicy::AutoIfReadOnly`] and
    ///   [`ApprovalPolicy::RequireApproval`] always require it except when
    ///   the risk class is [`ActionRisk::ReadOnly`].
    #[must_use]
    pub const fn requires_human(self, risk: ActionRisk) -> bool {
        match self {
            Self::AlwaysRequireApproval => true,
            Self::AutoIfReadOnly | Self::RequireApproval => !risk.is_read_only(),
        }
    }

    /// Whether this policy is one that can genuinely require approval
    /// (i.e. it **never** automatically bypasses actions with side effects).
    ///
    /// Used in validation: an external write
    /// ([`SkillPermission::WriteExternal`]) must not rely purely on
    /// read-only automation.
    #[must_use]
    pub const fn can_require_approval(self) -> bool {
        matches!(self, Self::RequireApproval | Self::AlwaysRequireApproval)
    }
}

/// Heuristically detects a string that looks like a secret.
///
/// Used for two purposes:
/// 1. **Manifest validation** — manifest text fields must not contain
///    secrets (keys/tokens).
/// 2. **Proof redaction** — the same heuristic is used to redact
///    secret-looking values from proof bundles.
///
/// Patterns detected:
/// - an `sk-` prefix followed by ≥8 word characters (OpenAI-style keys),
/// - AWS-style access key IDs (`AKIA` + 16 uppercase letters/digits),
/// - `Bearer <token>`-style Authorization values,
/// - long contiguous hex or base64 strings (≥32 characters),
/// - field-style disclosures such as `api_key=...`, `apikey: ...`,
///   `secret=...`, `password=...`, `token=...`.
///
/// The heuristic is deliberately cautious (false positives rather than
/// false negatives), since leaking a secret is worse than over-redacting.
#[must_use]
pub fn detect_secret_like(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }

    if has_sk_prefix(trimmed)
        || has_aws_access_key(trimmed)
        || has_bearer_token(trimmed)
        || has_long_token_run(trimmed)
    {
        return true;
    }

    has_secret_field_assignment(trimmed)
}

/// An `sk-` prefix followed by at least 8 word characters.
fn has_sk_prefix(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.split(|c: char| !is_token_char(c)).any(|chunk| {
        let lower_chunk = chunk;
        lower_chunk.starts_with("sk-") && lower_chunk.len() >= 3 + 8 && {
            lower_chunk[3..].chars().all(is_token_char)
        }
    })
}

/// AWS-style access key id: `AKIA` + 16 uppercase alphanumeric characters.
fn has_aws_access_key(value: &str) -> bool {
    value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|chunk| {
            chunk.len() == 20
                && chunk.starts_with("AKIA")
                && chunk[4..]
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        })
}

/// A `Bearer <token>`-style Authorization value (token ≥8 characters).
fn has_bearer_token(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let Some(pos) = lower.find("bearer ") else {
        return false;
    };
    let rest = value[pos + "bearer ".len()..].trim_start();
    let token: String = rest.chars().take_while(|&c| is_token_char(c)).collect();
    token.len() >= 8
}

/// A long contiguous hex- or base64-style string (≥32 characters).
fn has_long_token_run(value: &str) -> bool {
    value
        .split(|c: char| !is_base64_char(c))
        .any(|chunk| chunk.len() >= 32 && looks_high_entropy(chunk))
}

/// A field-style disclosure, e.g. `api_key=...`, `secret: ...`, `token=...`.
fn has_secret_field_assignment(value: &str) -> bool {
    const FIELD_NAMES: [&str; 6] = ["api_key", "apikey", "secret", "password", "token", "passwd"];
    let lower = value.to_ascii_lowercase();
    for name in FIELD_NAMES {
        let mut search_from = 0;
        while let Some(rel) = lower[search_from..].find(name) {
            let idx = search_from + rel;
            // Make sure the field name is preceded by a word boundary (not e.g. "mytoken").
            let boundary_ok = idx == 0
                || !lower.as_bytes()[idx - 1].is_ascii_alphanumeric()
                    && lower.as_bytes()[idx - 1] != b'_';
            let after = idx + name.len();
            if boundary_ok && assignment_has_value(&lower[after..]) {
                return true;
            }
            search_from = idx + name.len();
        }
    }
    false
}

/// Whether the field name is followed by `=`/`:` and at least one token
/// character as the value.
fn assignment_has_value(after: &str) -> bool {
    let after = after.trim_start();
    let Some(rest) = after.strip_prefix('=').or_else(|| after.strip_prefix(':')) else {
        return false;
    };
    let rest = rest.trim_start().trim_start_matches(['"', '\'']);
    rest.chars().next().is_some_and(is_token_char)
}

/// Whether the character is an allowed token character (alphanumeric, `-` or `_`).
const fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Whether the character is a typical base64/hex alphabet member (incl. `+`, `/`, `=`).
const fn is_base64_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=' || c == '-' || c == '_'
}

/// A rough entropy estimate: the string has both letters and digits, or it
/// is pure long hex. Prevents e.g. a long plain letter string (a word) from
/// being classified as a secret.
fn looks_high_entropy(chunk: &str) -> bool {
    let has_digit = chunk.chars().any(|c| c.is_ascii_digit());
    let has_alpha = chunk.chars().any(|c| c.is_ascii_alphabetic());
    let is_hex = chunk.chars().all(|c| c.is_ascii_hexdigit());
    (has_digit && has_alpha) || (is_hex && chunk.len() >= 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_serde_snake_case() {
        let json = serde_json::to_string(&SkillPermission::WriteExternal).expect("serialize");
        assert_eq!(json, "\"write_external\"");
        let back: SkillPermission = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, SkillPermission::WriteExternal);
    }

    #[test]
    fn risk_serde_snake_case() {
        let json = serde_json::to_string(&ActionRisk::ReadOnly).expect("serialize");
        assert_eq!(json, "\"read_only\"");
    }

    #[test]
    fn unknown_risk_is_rejected_by_serde() {
        let parsed: std::result::Result<ActionRisk, _> = serde_json::from_str("\"nuke_planet\"");
        assert!(parsed.is_err());
    }

    #[test]
    fn approval_policy_requires_human_logic() {
        assert!(!ApprovalPolicy::AutoIfReadOnly.requires_human(ActionRisk::ReadOnly));
        assert!(ApprovalPolicy::AutoIfReadOnly.requires_human(ActionRisk::WriteExternal));
        assert!(!ApprovalPolicy::RequireApproval.requires_human(ActionRisk::ReadOnly));
        assert!(ApprovalPolicy::RequireApproval.requires_human(ActionRisk::SpendMoney));
        assert!(ApprovalPolicy::AlwaysRequireApproval.requires_human(ActionRisk::ReadOnly));
    }

    #[test]
    fn can_require_approval_flags() {
        assert!(!ApprovalPolicy::AutoIfReadOnly.can_require_approval());
        assert!(ApprovalPolicy::RequireApproval.can_require_approval());
        assert!(ApprovalPolicy::AlwaysRequireApproval.can_require_approval());
    }

    #[test]
    fn read_only_may_auto_run_unless_forced() {
        assert_eq!(
            required_approval(ActionRisk::ReadOnly, ApprovalPolicy::AutoIfReadOnly),
            ApprovalRequirement::AutoRun
        );
        assert_eq!(
            required_approval(ActionRisk::ReadOnly, ApprovalPolicy::RequireApproval),
            ApprovalRequirement::AutoRun
        );
        // The policy forces approval even for a read-only action.
        assert_eq!(
            required_approval(ActionRisk::ReadOnly, ApprovalPolicy::AlwaysRequireApproval),
            ApprovalRequirement::RequireApproval
        );
    }

    #[test]
    fn write_external_requires_approval_by_default() {
        for policy in [
            ApprovalPolicy::AutoIfReadOnly,
            ApprovalPolicy::RequireApproval,
            ApprovalPolicy::AlwaysRequireApproval,
        ] {
            assert_eq!(
                required_approval(ActionRisk::WriteExternal, policy),
                ApprovalRequirement::RequireApproval,
                "write_external must always require approval (policy {policy:?})"
            );
        }
    }

    #[test]
    fn send_message_and_execute_code_require_approval() {
        for risk in [ActionRisk::SendMessage, ActionRisk::ExecuteCode] {
            for policy in [
                ApprovalPolicy::AutoIfReadOnly,
                ApprovalPolicy::RequireApproval,
                ApprovalPolicy::AlwaysRequireApproval,
            ] {
                assert!(
                    required_approval(risk, policy).requires_approval(),
                    "{risk:?} must require approval (policy {policy:?})"
                );
            }
        }
    }

    #[test]
    fn spend_money_and_irreversible_always_require_approval_even_if_auto() {
        // Fail-safe: even if the policy is the most permissive one, these must
        // never run automatically.
        for risk in [ActionRisk::SpendMoney, ActionRisk::Irreversible] {
            assert_eq!(
                required_approval(risk, ApprovalPolicy::AutoIfReadOnly),
                ApprovalRequirement::RequireApproval,
                "{risk:?} must fail safe to RequireApproval"
            );
            assert_eq!(
                required_approval(risk, ApprovalPolicy::RequireApproval),
                ApprovalRequirement::RequireApproval
            );
        }
    }

    #[test]
    fn write_local_auto_runs_only_under_require_approval_policy() {
        assert_eq!(
            required_approval(ActionRisk::WriteLocal, ApprovalPolicy::RequireApproval),
            ApprovalRequirement::AutoRun
        );
        assert_eq!(
            required_approval(ActionRisk::WriteLocal, ApprovalPolicy::AutoIfReadOnly),
            ApprovalRequirement::RequireApproval
        );
    }

    #[test]
    fn approval_requirement_serde_snake_case() {
        let json = serde_json::to_string(&ApprovalRequirement::AutoRun).expect("serialize");
        assert_eq!(json, "\"auto_run\"");
        let back: ApprovalRequirement = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, ApprovalRequirement::AutoRun);
    }

    #[test]
    fn detects_sk_prefix_secret() {
        // Built at runtime so there is no long literal in the source code.
        let fake = format!("sk-{}", "live".repeat(4));
        assert!(detect_secret_like(&fake));
    }

    #[test]
    fn detects_aws_access_key() {
        let fake = format!("AKIA{}", "A1B2C3D4E5F6G7H8");
        assert!(detect_secret_like(&fake));
    }

    #[test]
    fn detects_bearer_token() {
        let fake = format!("Authorization: Bearer {}", "abcd1234efgh");
        assert!(detect_secret_like(&fake));
    }

    #[test]
    fn detects_long_hex_run() {
        let fake = "a1b2".repeat(10); // 40 hex characters
        assert!(detect_secret_like(&fake));
    }

    #[test]
    fn detects_field_assignment() {
        assert!(detect_secret_like("api_key=abc123"));
        assert!(detect_secret_like("password: hunter2x"));
    }

    #[test]
    fn ignores_plain_text() {
        assert!(!detect_secret_like(
            "Lähettää tervehdysviestin kanavalle general."
        ));
        // Note: this is a Finnish test-data string literal being asserted
        // against, not a comment — left unchanged per instructions.
        assert!(!detect_secret_like(""));
        assert!(!detect_secret_like("mock-1"));
        assert!(!detect_secret_like("example-org/example-repo"));
    }
}
