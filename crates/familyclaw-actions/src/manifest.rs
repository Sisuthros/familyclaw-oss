//! Skill manifests: skill description, required permissions, risk class,
//! approval policy, and input/output hints (Layer A, generic — no
//! real providers, personas, or keys).
//!
//! A manifest can be parsed from both TOML ([`SkillManifest::from_toml`]) and
//! JSON ([`SkillManifest::from_json`]) format. Validation
//! ([`SkillManifest::validate`]) rejects:
//! - an empty or `nil` id,
//! - an empty name or version,
//! - values that look like secrets in any text field (including
//!   text values within the [`SkillManifest::input_schema`] schema),
//! - an [`SkillManifest::input_schema`] schema whose root is not a JSON object,
//! - external writes ([`SkillPermission::WriteExternal`]) without a policy
//!   that can genuinely require approval,
//! - an external skill's ([`SkillManifest::is_external`]) invalid or
//!   missing Ed25519 signature (fail-closed;
//!   [`SkillManifest::verify_external_signature`]).
//!
//! Unknown risk classes are already rejected by serde (enum validation).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ActionError, Result};
use crate::ids::SkillId;
use crate::policy::{detect_secret_like, ActionRisk, ApprovalPolicy, SkillPermission};

/// Module readiness state (scaffold compatibility).
///
/// Kept so that [`crate::all_modules_scaffolded`] keeps compiling.
pub(crate) const SCAFFOLDED: bool = true;

/// The manifest's default input schema: an empty JSON object `{"type":"object"}`.
///
/// Used in two places: as the serde deserialization default (older
/// stored manifests without an `input_schema` field load with this), and
/// as the starting value for [`SkillManifest`] builders. The root is ALWAYS
/// an object, so the schema is valid as-is for an LLM tool's `parameters` field.
#[must_use]
pub fn default_input_schema() -> Value {
    serde_json::json!({ "type": "object" })
}

/// A single skill's manifest: all the information the registry and policy
/// layer need before a skill can be planned or executed.
///
/// The manifest is pure data (no executable logic) and serializes
/// to TOML and JSON format without transformation.
///
/// `PartialEq` (not `Eq`) is due to the [`SkillManifest::input_schema`] field:
/// `serde_json::Value` implements only `PartialEq` (because of floating-point numbers).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillManifest {
    /// The skill's unique identifier in the registry.
    pub id: SkillId,
    /// Human-readable name (e.g. `send-greeting`).
    pub name: String,
    /// Version string (e.g. `1.0.0`); not enforced to follow semver format.
    pub version: String,
    /// Short description of what the skill does.
    pub description: String,
    /// Permissions (capabilities) the skill needs.
    pub permissions: Vec<SkillPermission>,
    /// The action's risk class.
    pub risk: ActionRisk,
    /// Approval policy (whether human approval is required).
    pub approval_policy: ApprovalPolicy,
    /// Free-form hint about the expected input shape (e.g. a schema name).
    ///
    /// Intended for human-readable display; the structured, machine-readable
    /// version is provided by [`SkillManifest::input_schema`].
    #[serde(default)]
    pub input_hint: Option<String>,
    /// Free-form hint about the expected output shape.
    #[serde(default)]
    pub output_hint: Option<String>,
    /// Machine-readable JSON Schema description of the skill's input.
    ///
    /// This lets the skill be advertised to an LLM as a genuine tool: the schema
    /// passes through as-is into the tool's `parameters` field. The root MUST be
    /// a JSON object (a scalar/array is not valid); validation
    /// ([`SkillManifest::validate`]) rejects anything else. When no value is given, serde
    /// fills it with the [`default_input_schema`] function (`{"type":"object"}`),
    /// so that older manifests stored without this field still load.
    #[serde(default = "default_input_schema")]
    pub input_schema: Value,
    /// Identifier of the external publisher (e.g. `mock_provider`). When set,
    /// the skill is **external** and requires an Ed25519 signature
    /// ([`SkillManifest::signature`]) and a trusted key
    /// (`FAMILYCLAW_SKILL_REGISTRY`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    /// Ed25519 signature (hex, 64 bytes) over the manifest's signing payload.
    /// Required for external skills; built-in Layer A skills leave
    /// this field empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl SkillManifest {
    /// Sets the machine-readable input schema and returns the modified manifest
    /// (builder-style chaining).
    ///
    /// The schema does not replace the [`SkillManifest::input_hint`] hint — both
    /// are kept: `input_hint` for human display, `input_schema` for the LLM. The value is NOT
    /// validated here; the root object requirement and the secret check are done
    /// only when [`SkillManifest::validate`] is called.
    ///
    /// # Examples
    /// ```
    /// use familyclaw_actions::manifest::SkillManifest;
    /// # use familyclaw_actions::ids::SkillId;
    /// # use familyclaw_actions::policy::{ActionRisk, ApprovalPolicy};
    /// # let base = SkillManifest {
    /// #     id: SkillId::new(),
    /// #     name: "demo".into(),
    /// #     version: "1.0.0".into(),
    /// #     description: "demo".into(),
    /// #     permissions: vec![],
    /// #     risk: ActionRisk::ReadOnly,
    /// #     approval_policy: ApprovalPolicy::AutoIfReadOnly,
    /// #     input_hint: None,
    /// #     output_hint: None,
    /// #     input_schema: serde_json::json!({ "type": "object" }),
    /// #     publisher: None,
    /// #     signature: None,
    /// # };
    /// let m = base.with_input_schema(serde_json::json!({
    ///     "type": "object",
    ///     "properties": { "text": { "type": "string" } },
    ///     "required": ["text"]
    /// }));
    /// assert_eq!(m.input_schema["type"], "object");
    /// ```
    #[must_use]
    pub fn with_input_schema(mut self, input_schema: Value) -> Self {
        self.input_schema = input_schema;
        self
    }

    /// Parses the manifest from a TOML string.
    ///
    /// # Errors
    /// Returns [`ActionError::ManifestParse`] if the TOML is invalid or
    /// does not match the manifest's structure (e.g. an unknown risk class).
    pub fn from_toml(input: &str) -> Result<Self> {
        toml::from_str(input).map_err(|e| ActionError::ManifestParse(e.to_string()))
    }

    /// Parses the manifest from a JSON string.
    ///
    /// # Errors
    /// Returns [`ActionError::ManifestParse`] if the JSON is invalid or
    /// does not match the manifest's structure (e.g. an unknown risk class).
    pub fn from_json(input: &str) -> Result<Self> {
        serde_json::from_str(input).map_err(|e| ActionError::ManifestParse(e.to_string()))
    }

    /// Serializes the manifest to a JSON string.
    ///
    /// # Errors
    /// Returns [`ActionError::ManifestParse`] if serialization fails.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|e| ActionError::ManifestParse(e.to_string()))
    }

    /// Validates the manifest's internal integrity and security rules.
    ///
    /// # Errors
    /// - [`ActionError::ManifestValidation`] if the id is `nil`, or the name/
    ///   version is empty, or the [`SkillManifest::input_schema`] schema's root is not
    ///   a JSON object, or an external write has a policy that does not
    ///   require approval.
    /// - [`ActionError::SecretInManifest`] if any text field or
    ///   the input schema's text value contains a value that looks like a secret.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_nil() {
            return Err(ActionError::ManifestValidation(
                "skill id missing (nil)".to_string(),
            ));
        }
        if self.name.trim().is_empty() {
            return Err(ActionError::ManifestValidation("name is empty".to_string()));
        }
        if self.version.trim().is_empty() {
            return Err(ActionError::ManifestValidation(
                "version is empty".to_string(),
            ));
        }

        // The input schema's root must be a JSON object for it to be valid as an LLM
        // tool's `parameters` field as-is (a scalar/array is not).
        if !self.input_schema.is_object() {
            return Err(ActionError::ManifestValidation(
                "input_schema root must be a JSON object".to_string(),
            ));
        }

        // Security check: no text field may contain a secret.
        for (field, value) in self.text_fields() {
            if detect_secret_like(value) {
                return Err(ActionError::SecretInManifest(format!(
                    "field '{field}' appears to contain a secret"
                )));
            }
        }

        // Same security check for the input schema: the schema is structured, so
        // all of its string nodes (keys and values) are walked.
        if let Some(secret_path) = first_secret_in_json(&self.input_schema, "input_schema") {
            return Err(ActionError::SecretInManifest(format!(
                "input_schema path '{secret_path}' appears to contain a secret"
            )));
        }

        // External writes require a policy that can genuinely require approval.
        if self.permissions.contains(&SkillPermission::WriteExternal)
            && !self.approval_policy.can_require_approval()
        {
            return Err(ActionError::ManifestValidation(
                "write_external vaatii hyväksyntää edellyttävän approval_policy-arvon \
                 (require_approval tai always_require_approval)"
                    .to_string(),
            ));
        }

        // Permission <-> risk class cross-check (defense in depth).
        //
        // The pipeline derives the approval requirement from the risk class
        // ([`crate::policy::required_approval`]). If a high-risk permission
        // (e.g. spending money) were labeled with an auto-runnable risk class
        // (read_only / write_local), the pipeline would run it WITHOUT approval.
        // This is prevented here: the invariant is "spend_money and irreversible always
        // require approval, even if the manifest tries to auto-run".
        for perm in &self.permissions {
            // Spending money must not be disguised as a lighter risk.
            if perm.requires_spend_money_risk() && self.risk != ActionRisk::SpendMoney {
                return Err(ActionError::ManifestValidation(format!(
                    "permission 'spend_money' requires risk = spend_money (was {:?}) — \
                     spending money must not bypass approval via the wrong risk class",
                    self.risk
                )));
            }
            // Permissions with side effects must not be in an auto-runnable
            // risk class (read_only / write_local).
            if perm.forbids_auto_run_risk() && self.risk.is_auto_runnable_class() {
                return Err(ActionError::ManifestValidation(format!(
                    "permission {perm:?} must not be in the auto-runnable risk class {:?} — \
                     it would bypass the required approval",
                    self.risk
                )));
            }
        }

        self.verify_external_signature()?;

        Ok(())
    }

    /// Is the manifest **external** (a third-party skill)?
    ///
    /// External = `publisher` is set and non-empty. Built-in Layer A
    /// skills do not set a publisher and do not require a signature.
    #[must_use]
    pub fn is_external(&self) -> bool {
        self.publisher
            .as_ref()
            .is_some_and(|publisher| !publisher.trim().is_empty())
    }

    /// Verifies an external skill's Ed25519 signature against the trusted key
    /// registry (`FAMILYCLAW_SKILL_REGISTRY`).
    ///
    /// Built-in skills (no `publisher` field) are skipped. For external
    /// skills, an invalid or missing signature is **fail-closed**
    /// ([`ActionError::SignatureInvalid`]).
    ///
    /// # Errors
    /// [`ActionError::SignatureInvalid`] if the signature is missing, the registry
    /// is not found, the publisher is not trusted, or verification fails.
    pub fn verify_external_signature(&self) -> Result<()> {
        if !self.is_external() {
            return Ok(());
        }

        let publisher = self.publisher.as_ref().expect("is_external checked");
        let signature_hex = self
            .signature
            .as_ref()
            .filter(|sig| !sig.trim().is_empty())
            .ok_or_else(|| {
                ActionError::SignatureInvalid(format!(
                    "external skill '{publisher}' missing signature field"
                ))
            })?;

        let registry_path = std::env::var("FAMILYCLAW_SKILL_REGISTRY").map_err(|_| {
            ActionError::SignatureInvalid(
                "FAMILYCLAW_SKILL_REGISTRY not set — cannot verify external skill".to_string(),
            )
        })?;

        let trusted_keys = load_trusted_skill_keys(Path::new(&registry_path))?;
        let public_key_hex = trusted_keys.get(publisher).ok_or_else(|| {
            ActionError::SignatureInvalid(format!(
                "publisher '{publisher}' not in trusted skill registry"
            ))
        })?;

        let payload = self.signing_payload()?;
        verify_ed25519_signature(public_key_hex, signature_hex, &payload)?;

        Ok(())
    }

    /// Returns the manifest's signing payload (JSON without the `signature` field).
    fn signing_payload(&self) -> Result<Vec<u8>> {
        let mut unsigned = self.clone();
        unsigned.signature = None;
        serde_json::to_vec(&unsigned)
            .map_err(|e| ActionError::SignatureInvalid(format!("signing payload encode: {e}")))
    }

    /// Returns the text fields to validate as (field name, value) pairs.
    fn text_fields(&self) -> Vec<(&'static str, &str)> {
        let mut fields = vec![
            ("name", self.name.as_str()),
            ("version", self.version.as_str()),
            ("description", self.description.as_str()),
        ];
        if let Some(hint) = &self.input_hint {
            fields.push(("input_hint", hint.as_str()));
        }
        if let Some(hint) = &self.output_hint {
            fields.push(("output_hint", hint.as_str()));
        }
        if let Some(publisher) = &self.publisher {
            fields.push(("publisher", publisher.as_str()));
        }
        // `signature` is left out: a hex signature can look
        // like a secret to `detect_secret_like`, but it is verified
        // separately by [`SkillManifest::verify_external_signature`].
        fields
    }
}

/// Trusted publishers' Ed25519 public keys (hex, 32 bytes).
///
/// JSON format: a flat object `{ "publisher_id": "hex_pubkey", ... }`.
type TrustedSkillKeys = HashMap<String, String>;

/// Reads the trusted skill keys from the `FAMILYCLAW_SKILL_REGISTRY` path.
fn load_trusted_skill_keys(path: &Path) -> Result<TrustedSkillKeys> {
    let raw = fs::read_to_string(path).map_err(|e| {
        ActionError::SignatureInvalid(format!(
            "FAMILYCLAW_SKILL_REGISTRY read failed ({}): {e}",
            path.display()
        ))
    })?;
    serde_json::from_str(&raw).map_err(|e| {
        ActionError::SignatureInvalid(format!(
            "FAMILYCLAW_SKILL_REGISTRY parse failed ({}): {e}",
            path.display()
        ))
    })
}

/// Verifies an Ed25519 signature using a hex key and hex signature.
fn verify_ed25519_signature(
    public_key_hex: &str,
    signature_hex: &str,
    message: &[u8],
) -> Result<()> {
    let pk_bytes = decode_hex(public_key_hex).map_err(|e| {
        ActionError::SignatureInvalid(format!("trusted public key hex invalid: {e}"))
    })?;
    let sig_bytes = decode_hex(signature_hex)
        .map_err(|e| ActionError::SignatureInvalid(format!("signature hex invalid: {e}")))?;

    let pk_arr: [u8; 32] = pk_bytes
        .try_into()
        .map_err(|_| ActionError::SignatureInvalid("public key must be 32 bytes".to_string()))?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| ActionError::SignatureInvalid("signature must be 64 bytes".to_string()))?;

    let verifying_key = VerifyingKey::from_bytes(&pk_arr)
        .map_err(|e| ActionError::SignatureInvalid(format!("invalid Ed25519 public key: {e}")))?;
    let signature = Signature::from_bytes(&sig_arr);

    verifying_key
        .verify_strict(message, &signature)
        .map_err(|_| {
            ActionError::SignatureInvalid("Ed25519 signature verification failed".to_string())
        })
}

/// Decodes a hex string into bytes (allows an optional `0x` prefix).
fn decode_hex(input: &str) -> std::result::Result<Vec<u8>, String> {
    let trimmed = input
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    if !trimmed.len().is_multiple_of(2) {
        return Err("odd hex length".to_string());
    }
    (0..trimmed.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&trimmed[i..i + 2], 16)
                .map_err(|e| format!("invalid hex at offset {i}: {e}"))
        })
        .collect()
}

/// Finds the first secret-looking string in a JSON value.
///
/// Recursively walks objects' keys and values as well as arrays' elements,
/// and returns the [`Some`] path (e.g. `input_schema.properties.token`)
/// to the first node whose text value passes the
/// [`detect_secret_like`] check. Returns [`None`] if no secrets are
/// found. `path` is the root prefix supplied by the caller (e.g. `"input_schema"`).
///
/// This extends the manifest's secret-free guarantee to also cover the
/// structured [`SkillManifest::input_schema`] schema, not just flat
/// text fields.
fn first_secret_in_json(value: &Value, path: &str) -> Option<String> {
    match value {
        Value::String(s) => {
            if detect_secret_like(s) {
                Some(path.to_string())
            } else {
                None
            }
        }
        Value::Object(map) => map.iter().find_map(|(key, child)| {
            if detect_secret_like(key) {
                return Some(format!("{path}.{key} (avain)"));
            }
            first_secret_in_json(child, &format!("{path}.{key}"))
        }),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(idx, child)| first_secret_in_json(child, &format!("{path}[{idx}]"))),
        // Numbers, booleans, and null cannot be secrets.
        Value::Number(_) | Value::Bool(_) | Value::Null => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TEST FIX 2026-07-09 (audit): FAMILYCLAW_SKILL_REGISTRY is a process-
    // global env var. Two tests set_var/remove_var it -> when run in parallel,
    // one test's remove_var wipes out the other's set_var and validation fails
    // intermittently (flaky CI). Serialized with this lock.
    static REGISTRY_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Helper: builds a valid base manifest with the given id.
    fn valid_manifest() -> SkillManifest {
        SkillManifest {
            id: SkillId::new(),
            name: "send-greeting".to_string(),
            version: "1.0.0".to_string(),
            description: "Lähettää tervehdysviestin kanavalle general.".to_string(),
            permissions: vec![SkillPermission::SendMessage],
            risk: ActionRisk::SendMessage,
            approval_policy: ApprovalPolicy::RequireApproval,
            input_hint: Some("text".to_string()),
            output_hint: Some("ack".to_string()),
            input_schema: default_input_schema(),
            publisher: None,
            signature: None,
        }
    }

    #[test]
    fn valid_manifest_loads_from_toml() {
        let id = SkillId::new();
        let toml_src = format!(
            r#"
id = "{id}"
name = "read-doc"
version = "1.2.0"
description = "Lukee paikallisen dokumentin."
permissions = ["read_files"]
risk = "read_only"
approval_policy = "auto_if_read_only"
"#
        );
        let manifest = SkillManifest::from_toml(&toml_src).expect("toml parses");
        assert_eq!(manifest.id, id);
        assert_eq!(manifest.name, "read-doc");
        assert_eq!(manifest.risk, ActionRisk::ReadOnly);
        // input_schema was missing from the source -> the serde default fills it in.
        assert_eq!(manifest.input_schema, default_input_schema());
        manifest.validate().expect("valid manifest validates");
    }

    #[test]
    fn valid_manifest_loads_from_json() {
        let id = SkillId::new();
        let json_src = format!(
            r#"{{
                "id": "{id}",
                "name": "read-doc",
                "version": "1.2.0",
                "description": "Lukee paikallisen dokumentin.",
                "permissions": ["read_files"],
                "risk": "read_only",
                "approval_policy": "auto_if_read_only"
            }}"#
        );
        let manifest = SkillManifest::from_json(&json_src).expect("json parses");
        assert_eq!(manifest.id, id);
        assert_eq!(manifest.approval_policy, ApprovalPolicy::AutoIfReadOnly);
        // An old serialized manifest without input_schema still loads.
        assert_eq!(manifest.input_schema, default_input_schema());
        manifest.validate().expect("valid manifest validates");
    }

    #[test]
    fn invalid_id_rejected() {
        let mut m = valid_manifest();
        m.id = SkillId::nil();
        let err = m.validate().expect_err("nil id must be rejected");
        assert!(matches!(err, ActionError::ManifestValidation(_)));
    }

    #[test]
    fn empty_name_rejected() {
        let mut m = valid_manifest();
        m.name = "   ".to_string();
        assert!(matches!(
            m.validate(),
            Err(ActionError::ManifestValidation(_))
        ));
    }

    #[test]
    fn unknown_risk_rejected() {
        let id = SkillId::new();
        let json_src = format!(
            r#"{{
                "id": "{id}",
                "name": "x",
                "version": "1.0.0",
                "description": "d",
                "permissions": [],
                "risk": "nuke_planet",
                "approval_policy": "require_approval"
            }}"#
        );
        let parsed = SkillManifest::from_json(&json_src);
        assert!(matches!(parsed, Err(ActionError::ManifestParse(_))));
    }

    #[test]
    fn write_external_without_approval_rejected() {
        let mut m = valid_manifest();
        m.permissions = vec![SkillPermission::WriteExternal];
        m.risk = ActionRisk::WriteExternal;
        m.approval_policy = ApprovalPolicy::AutoIfReadOnly;
        let err = m
            .validate()
            .expect_err("write_external without approval rejected");
        assert!(matches!(err, ActionError::ManifestValidation(_)));

        // The same manifest with an approval-requiring policy passes.
        m.approval_policy = ApprovalPolicy::RequireApproval;
        m.validate()
            .expect("write_external with approval validates");
    }

    /// INVARIANT (adversarial): a manifest must NOT declare the spend-money permission
    /// ([`SkillPermission::SpendMoney`]) while labeling the risk class as something
    /// that bypasses approval (e.g. [`ActionRisk::ReadOnly`]). Otherwise the pipeline
    /// would derive `required_approval(ReadOnly, AutoIfReadOnly) == AutoRun` and
    /// a money-spending skill would run without approval.
    #[test]
    fn spend_money_permission_mislabeled_as_low_risk_rejected() {
        let mut m = valid_manifest();
        m.permissions = vec![SkillPermission::SpendMoney];
        m.risk = ActionRisk::ReadOnly;
        m.approval_policy = ApprovalPolicy::AutoIfReadOnly;
        let err = m
            .validate()
            .expect_err("spend_money mislabeled as read_only must be rejected");
        assert!(matches!(err, ActionError::ManifestValidation(_)));

        // Correctly labeled (risk = SpendMoney), the same permission validates.
        m.risk = ActionRisk::SpendMoney;
        m.validate()
            .expect("spend_money with matching risk validates");
    }

    /// INVARIANT (adversarial): the write-external permission
    /// ([`SkillPermission::WriteExternal`]) labeled as a read risk instead of
    /// an irreversible one must not pass — an irreversible/external side effect must
    /// not auto-run.
    #[test]
    fn write_external_permission_mislabeled_as_read_only_rejected() {
        let mut m = valid_manifest();
        m.permissions = vec![SkillPermission::WriteExternal];
        m.risk = ActionRisk::ReadOnly;
        // A policy that can genuinely require approval, so that the earlier
        // write_external check is NOT what rejects it — proving that
        // specifically the risk-class cross-check is what bites.
        m.approval_policy = ApprovalPolicy::RequireApproval;
        let err = m
            .validate()
            .expect_err("write_external mislabeled as read_only must be rejected");
        assert!(matches!(err, ActionError::ManifestValidation(_)));
    }

    #[test]
    fn manifest_with_secret_value_rejected() {
        let mut m = valid_manifest();
        // The secret is built at runtime so there's no long literal in the source.
        let fake = format!("sk-{}", "live".repeat(4));
        m.description = format!("Käyttää avainta {fake}");
        let err = m.validate().expect_err("secret-looking value rejected");
        assert!(matches!(err, ActionError::SecretInManifest(_)));
    }

    #[test]
    fn json_roundtrip_preserves_manifest() {
        let m = valid_manifest();
        let json = m.to_json().expect("serialize");
        let back = SkillManifest::from_json(&json).expect("deserialize");
        assert_eq!(m, back);
    }

    #[test]
    fn json_roundtrip_preserves_custom_input_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "viesti" },
                "count": { "type": "integer", "minimum": 0 }
            },
            "required": ["text"]
        });
        let m = valid_manifest().with_input_schema(schema.clone());
        m.validate().expect("custom schema validates");
        let back = SkillManifest::from_json(&m.to_json().expect("serialize")).expect("deserialize");
        assert_eq!(back.input_schema, schema);
        assert_eq!(m, back);
    }

    #[test]
    fn default_input_schema_is_empty_object() {
        let schema = default_input_schema();
        assert!(schema.is_object());
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn with_input_schema_keeps_input_hint() {
        let m = valid_manifest().with_input_schema(serde_json::json!({ "type": "object" }));
        // input_hint is kept for human display alongside the schema.
        assert_eq!(m.input_hint.as_deref(), Some("text"));
    }

    #[test]
    fn non_object_input_schema_rejected() {
        let mut m = valid_manifest();
        m.input_schema = serde_json::json!("not an object");
        let err = m
            .validate()
            .expect_err("scalar input_schema root must be rejected");
        assert!(matches!(err, ActionError::ManifestValidation(_)));

        m.input_schema = serde_json::json!([1, 2, 3]);
        let err = m
            .validate()
            .expect_err("array input_schema root must be rejected");
        assert!(matches!(err, ActionError::ManifestValidation(_)));
    }

    #[test]
    fn secret_in_input_schema_value_rejected() {
        let mut m = valid_manifest();
        // The secret is built at runtime (no long literal in the source).
        let fake = format!("sk-{}", "live".repeat(4));
        m.input_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "key": { "type": "string", "default": fake }
            }
        });
        let err = m
            .validate()
            .expect_err("secret-looking value inside schema must be rejected");
        assert!(matches!(err, ActionError::SecretInManifest(_)));
    }

    #[test]
    fn external_skill_without_signature_rejected() {
        let mut m = valid_manifest();
        m.publisher = Some("mock_provider".to_string());
        let err = m
            .validate()
            .expect_err("external skill without signature must fail closed");
        assert!(matches!(err, ActionError::SignatureInvalid(_)));
    }

    #[test]
    fn external_skill_with_valid_signature_accepted() {
        use ed25519_dalek::{Signer, SigningKey};
        use std::io::Write;
        // Serializes changes to the global REGISTRY env var (flaky-race fix).
        let _env_guard = REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let verifying_key = signing_key.verifying_key();
        let public_key_hex = bytes_to_hex(&verifying_key.to_bytes());

        let mut m = valid_manifest();
        m.publisher = Some("mock_provider".to_string());

        let payload = m.signing_payload().expect("signing payload");
        let signature = signing_key.sign(&payload);
        m.signature = Some(bytes_to_hex(&signature.to_bytes()));

        let dir = std::env::temp_dir().join(format!("fc-skill-reg-{}", SkillId::new()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let registry_path = dir.join("trusted_keys.json");
        let registry_json = format!(r#"{{"mock_provider":"{public_key_hex}"}}"#);
        {
            let mut file = std::fs::File::create(&registry_path).expect("registry file");
            file.write_all(registry_json.as_bytes())
                .expect("write registry");
        }

        std::env::set_var("FAMILYCLAW_SKILL_REGISTRY", &registry_path);
        let result = m.validate();
        std::env::remove_var("FAMILYCLAW_SKILL_REGISTRY");
        let _ = std::fs::remove_dir_all(dir);

        result.expect("valid external signature must verify");
    }

    #[test]
    fn external_skill_with_tampered_signature_rejected() {
        use ed25519_dalek::SigningKey;
        use std::io::Write;
        // Serializes changes to the global REGISTRY env var (flaky-race fix).
        let _env_guard = REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let signing_key = SigningKey::from_bytes(&[9u8; 32]);
        let public_key_hex = bytes_to_hex(&signing_key.verifying_key().to_bytes());

        let mut m = valid_manifest();
        m.publisher = Some("mock_provider".to_string());
        m.signature = Some("00".repeat(64));

        let dir = std::env::temp_dir().join(format!("fc-skill-reg-{}", SkillId::new()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let registry_path = dir.join("trusted_keys.json");
        let registry_json = format!(r#"{{"mock_provider":"{public_key_hex}"}}"#);
        {
            let mut file = std::fs::File::create(&registry_path).expect("registry file");
            file.write_all(registry_json.as_bytes())
                .expect("write registry");
        }

        std::env::set_var("FAMILYCLAW_SKILL_REGISTRY", &registry_path);
        let err = m
            .validate()
            .expect_err("tampered signature must fail closed");
        std::env::remove_var("FAMILYCLAW_SKILL_REGISTRY");
        let _ = std::fs::remove_dir_all(dir);

        assert!(matches!(err, ActionError::SignatureInvalid(_)));
    }

    fn bytes_to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut acc, byte| {
            let _ = write!(acc, "{byte:02x}");
            acc
        })
    }
}
