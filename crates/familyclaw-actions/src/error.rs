//! Toimintopinon crate-paikallinen virhetyyppi [`ActionError`].
//!
//! `familyclaw-core` tarjoaa keskitetyn [`FamilyClawError`]-tyypin koko
//! alustalle. Tämä crate määrittelee oman, hienojakoisemman virhetyyppinsä
//! toimintopinon (observe→plan→approve→execute→verify→prove→remember→report)
//! erityistilanteille, ja muuntaa sen tarvittaessa keskitettyyn tyyppiin
//! [`From`]-toteutuksilla molempiin suuntiin.
//!
//! Tuotantopolulla EI käytetä `unwrap()`/`expect()`/`panic!()` — kaikki
//! virheet kulkevat [`Result`]-tyypin kautta.

use familyclaw_core::FamilyClawError;
use thiserror::Error;

/// Toimintopinon virhetyyppi.
///
/// `#[non_exhaustive]` jotta uusia variantteja voi lisätä myöhemmin
/// rikkomatta downstream-koodia.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ActionError {
    /// Skill-manifestin jäsennys epäonnistui (esim. kelvoton JSON).
    #[error("manifest parse error: {0}")]
    ManifestParse(String),

    /// Skill-manifestin validointi epäonnistui (puuttuva kenttä, virheellinen arvo).
    #[error("manifest validation error: {0}")]
    ManifestValidation(String),

    /// Manifestissa havaittiin salaisuudelta näyttävä arvo (ei sallittu).
    #[error("secret detected in manifest: {0}")]
    SecretInManifest(String),

    /// Ulkoisen taidon Ed25519-allekirjoituksen verifiointi epäonnistui.
    #[error("skill signature invalid: {0}")]
    SignatureInvalid(String),

    /// Viitattua taitoa ei löytynyt rekisteristä.
    #[error("unknown skill: {0}")]
    UnknownSkill(String),

    /// Viitattua entiteettiä (esim. tehtävää) ei löytynyt.
    #[error("not found: {0}")]
    NotFound(String),

    /// Tilakoneen siirtymä ei ollut laillinen.
    #[error("illegal transition: {0}")]
    IllegalTransition(String),

    /// Vaadittu hyväksyntä puuttuu kokonaan.
    #[error("approval missing: {0}")]
    ApprovalMissing(String),

    /// Hyväksyntä on vanhentunut (TTL ylittynyt).
    #[error("approval expired: {0}")]
    ApprovalExpired(String),

    /// Hyväksyntää on jo käytetty (toistokäyttö estetty, nonce kulutettu).
    #[error("approval reused: {0}")]
    ApprovalReused(String),

    /// Hyväksynnän payload-tiiviste ei vastaa suoritettavaa payloadia.
    #[error("approval payload mismatch: {0}")]
    ApprovalPayloadMismatch(String),

    /// Käytäntö (policy) esti toiminnon.
    #[error("policy denied: {0}")]
    PolicyDenied(String),

    /// Toiminnon suoritus epäonnistui.
    #[error("execution failed: {0}")]
    ExecutionFailed(String),

    /// Todistepaketin rakentaminen tai validointi epäonnistui.
    #[error("proof error: {0}")]
    Proof(String),

    /// MCP-työkalua ei löytynyt.
    #[error("mcp unknown tool: {0}")]
    McpUnknownTool(String),

    /// MCP-työkalun käyttö estettiin (esim. capability puuttuu).
    #[error("mcp denied: {0}")]
    McpDenied(String),

    /// Käärii alla olevan keskitetyn [`FamilyClawError`]-virheen
    /// (esim. IO, serde, config) ilman tiedonhukkaa.
    #[error(transparent)]
    Core(#[from] FamilyClawError),
}

/// Toimintopinon vakiotulostyyppi: [`std::result::Result`] jonka virhe on
/// aina [`ActionError`].
pub type Result<T> = std::result::Result<T, ActionError>;

impl From<ActionError> for FamilyClawError {
    /// Muuntaa toimintopinon virheen takaisin keskitettyyn tyyppiin.
    ///
    /// Jo keskitetystä tyypistä käärityt virheet ([`ActionError::Core`])
    /// puretaan sellaisinaan; muut variantit kartoitetaan
    /// [`FamilyClawError::InvalidInput`]-variantiksi säilyttäen viesti.
    fn from(err: ActionError) -> Self {
        match err {
            ActionError::Core(inner) => inner,
            other => FamilyClawError::invalid_input(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_format_with_context() {
        assert_eq!(
            ActionError::UnknownSkill("skill_a".into()).to_string(),
            "unknown skill: skill_a"
        );
        assert_eq!(
            ActionError::PolicyDenied("not allowed".into()).to_string(),
            "policy denied: not allowed"
        );
        assert_eq!(
            ActionError::ApprovalExpired("ttl 0".into()).to_string(),
            "approval expired: ttl 0"
        );
    }

    #[test]
    fn core_error_converts_into_action_error() {
        let core = FamilyClawError::not_found("agent_a");
        let action: ActionError = core.into();
        assert!(matches!(action, ActionError::Core(_)));
    }

    #[test]
    fn action_error_converts_into_core_error() {
        let action = ActionError::McpDenied("no capability".into());
        let core: FamilyClawError = action.into();
        assert!(matches!(core, FamilyClawError::InvalidInput(_)));
        assert!(core.to_string().contains("mcp denied"));
    }

    #[test]
    fn core_wrapped_unwraps_back_to_core() {
        let original = FamilyClawError::memory("decay failed");
        let action: ActionError = ActionError::Core(original);
        let core: FamilyClawError = action.into();
        assert!(matches!(core, FamilyClawError::Memory(_)));
    }

    #[test]
    fn result_alias_is_usable() {
        fn maybe(fail: bool) -> Result<u8> {
            if fail {
                Err(ActionError::ExecutionFailed("boom".into()))
            } else {
                Ok(7)
            }
        }
        assert_eq!(maybe(false).expect("ok"), 7);
        assert!(maybe(true).is_err());
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<ActionError>();
    }
}
