//! TOML-based configuration (Layer B).
//! Reads `~/.config/familyclaw/familyclaw.toml` + env overrides.

use familyclaw_core::FamilyClawError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const CONFIG_FILE_NAME: &str = "familyclaw.toml";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
#[derive(Default)]
pub struct FamilyConfig {
    pub agent: AgentCfg,
    /// Optional multi-agent list (`[[agents]]` in TOML). Empty → only the
    /// [`FamilyConfig::agent`] field is used (backward-compatible single agent).
    #[serde(default)]
    pub agents: Vec<AgentCfg>,
    pub channel: ChannelCfg,
    pub provider: ProviderCfg,
    pub memory: MemoryCfg,
    pub security: SecurityCfg,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentCfg {
    pub name: String,
    /// Optional per-agent reply target (overrides the channel default).
    #[serde(default)]
    pub reply_target: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ChannelCfg {
    pub kind: String,
    pub reply_target: String,
    pub discord: DiscordCfg,
    pub telegram: TelegramCfg,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DiscordCfg {
    pub webhook_url: String,
    pub channel_id: String,
    /// Ed25519 public key (hex) Discord Interactions -verifyyn.
    pub public_key: String,
    /// Discord bot token. If set, the gateway uses a two-way serenity bot
    /// connection (listens + posts) instead of webhook posting.
    pub bot_token: String,
    /// The operator's Discord user id. Only this id may DM the agent
    /// (one-on-one conversation). 0 = unset → DMs are dropped from everyone
    /// (safe default, never "all DMs allowed"). From the TOML field or the
    /// `FAMILYCLAW_OWNER_ID` env override.
    pub owner_id: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
#[derive(Default)]
pub struct TelegramCfg {
    pub token: String,
    pub channel_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ProviderCfg {
    pub kind: String,
    pub model: String,
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct MemoryCfg {
    pub retention_hours: u64,
    pub max_working_memories: usize,
    pub compaction_threshold: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SecurityCfg {
    pub profile: String,
    /// Optional bearer token used to protect `POST /inject`. Empty = no
    /// token → loopback-only default behavior (open). When set, `/inject`
    /// requires `Authorization: Bearer <token>` and rejects mismatches with 401.
    pub gateway_token: String,
}

// Defaults
impl Default for AgentCfg {
    fn default() -> Self {
        Self {
            name: "agent_a".into(),
            reply_target: String::new(),
        }
    }
}
impl Default for ChannelCfg {
    fn default() -> Self {
        Self {
            kind: "telegram".into(),
            reply_target: String::new(),
            discord: DiscordCfg::default(),
            telegram: TelegramCfg::default(),
        }
    }
}
impl Default for DiscordCfg {
    fn default() -> Self {
        Self {
            webhook_url: String::new(),
            channel_id: "discord-main".into(),
            public_key: String::new(),
            bot_token: String::new(),
            // 0 = no operator set → DMs off (safe default).
            owner_id: 0,
        }
    }
}
impl Default for ProviderCfg {
    fn default() -> Self {
        Self {
            kind: "openai".into(),
            // Provider-prefixed (`provider/model`) form: the resolver requires
            // it, otherwise a bare name is interpreted as the provider name →
            // fails to resolve → the agent goes mute (no text replies). See build_llm_chain.
            model: "openai/gpt-4.1-mini".into(),
            api_key: String::new(),
        }
    }
}
impl Default for MemoryCfg {
    fn default() -> Self {
        Self {
            retention_hours: 168,
            max_working_memories: 500,
            compaction_threshold: 0.3,
        }
    }
}
impl Default for SecurityCfg {
    fn default() -> Self {
        Self {
            profile: "supervised".into(),
            gateway_token: String::new(),
        }
    }
}

impl FamilyConfig {
    pub fn load() -> Result<Self, FamilyClawError> {
        let path = Self::find_path();
        let mut cfg = match &path {
            p if p.exists() => {
                let s = std::fs::read_to_string(p).map_err(|e| {
                    FamilyClawError::invalid_input(format!("read {}: {e}", p.display()))
                })?;
                toml::from_str(&s).map_err(|e| {
                    FamilyClawError::invalid_input(format!("parse {}: {e}", p.display()))
                })?
            }
            _ => {
                tracing::warn!("no config file found, using defaults + env overrides");
                FamilyConfig::default()
            }
        };
        cfg.apply_env();
        if path.exists() {
            tracing::info!(config=%path.display(), agent=%cfg.agent.name, "loaded familyclaw config");
        }
        Ok(cfg)
    }

    pub fn find_path() -> PathBuf {
        if let Ok(p) = std::env::var("FAMILYCLAW_CONFIG") {
            return PathBuf::from(p);
        }
        for b in [
            std::env::var("XDG_CONFIG_HOME").ok(),
            std::env::var("HOME").map(|h| format!("{h}/.config")).ok(),
        ]
        .into_iter()
        .flatten()
        {
            let p = PathBuf::from(&b).join("familyclaw").join(CONFIG_FILE_NAME);
            if p.exists() {
                return p;
            }
        }
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
            .join(".config")
            .join("familyclaw")
            .join(CONFIG_FILE_NAME)
    }

    fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("FAMILYCLAW_AGENT_NAME") {
            self.agent.name = v;
        }
        if let Ok(v) = std::env::var("FAMILYCLAW_CHANNEL_KIND") {
            self.channel.kind = v;
        }
        // Reply target: canonical `FAMILYCLAW_REPLY_TARGET` (same name as
        // .env.example, docs/RUNBOOK_WINDOWS.md, and main.rs's REPLY_TARGET_ENV).
        // `FAMILYCLAW_CHANNEL_REPLY_TARGET` is kept as a deprecated alias for
        // backward compatibility — read ONLY if the canonical one is not set
        // (the canonical one wins if both are set).
        if let Ok(v) = std::env::var("FAMILYCLAW_REPLY_TARGET") {
            self.channel.reply_target = v;
        } else if let Ok(v) = std::env::var("FAMILYCLAW_CHANNEL_REPLY_TARGET") {
            tracing::warn!(
                "FAMILYCLAW_CHANNEL_REPLY_TARGET is deprecated — use FAMILYCLAW_REPLY_TARGET"
            );
            self.channel.reply_target = v;
        }
        if let Ok(v) = std::env::var("FAMILYCLAW_PROVIDER_API_KEY") {
            self.provider.api_key = v;
        }
        if let Ok(v) = std::env::var("FAMILYCLAW_PROVIDER_MODEL") {
            self.provider.model = v;
        }
        if let Ok(v) = std::env::var("FAMILYCLAW_SECURITY_PROFILE") {
            self.security.profile = v;
        }
        if let Ok(v) = std::env::var("FAMILYCLAW_GATEWAY_TOKEN") {
            self.security.gateway_token = v;
        }
        if let Ok(v) = std::env::var("DISCORD_WEBHOOK_URL") {
            self.channel.discord.webhook_url = v;
        }
        if let Ok(v) = std::env::var("DISCORD_CHANNEL_ID") {
            self.channel.discord.channel_id = v;
        }
        if let Ok(v) = std::env::var("DISCORD_PUBLIC_KEY") {
            self.channel.discord.public_key = v;
        }
        if let Ok(v) = std::env::var("DISCORD_BOT_TOKEN") {
            self.channel.discord.bot_token = v;
        }
        // Operator's Discord user id for the DM gate. An invalid value does
        // NOT override the safe default: a warning is logged and the
        // TOML/default value is kept (never "all DMs allowed"). An empty
        // string also parses as invalid → warning, but an empty env value is rare.
        if let Ok(v) = std::env::var("FAMILYCLAW_OWNER_ID") {
            if let Ok(n) = v.trim().parse::<u64>() {
                self.channel.discord.owner_id = n;
            } else {
                tracing::warn!(
                    value = %v,
                    "FAMILYCLAW_OWNER_ID is not a valid u64 — ignoring env override, \
                     keeping configured owner_id (DMs stay disabled if owner_id is 0)"
                );
            }
        }
        if let Ok(v) = std::env::var("TELEGRAM_BOT_TOKEN") {
            self.channel.telegram.token = v;
        }
        if let Ok(v) = std::env::var("FAMILYCLAW_TELEGRAM_CHANNEL_ID") {
            self.channel.telegram.channel_id = v;
        }
        if let Ok(v) = std::env::var("FAMILYCLAW_MEMORY_RETENTION_HOURS") {
            if let Ok(n) = v.parse() {
                self.memory.retention_hours = n;
            }
        }
    }
}

// Accessor helpers
impl FamilyConfig {
    // Part of the public accessor surface alongside `model()`/`all_agents()`.
    // Current code reads the `agent.name` field directly, so this method is
    // not yet called — kept for API symmetry.
    #[allow(dead_code)]
    pub fn agent_name(&self) -> &str {
        &self.agent.name
    }

    /// Returns all serve agents: `[[agents]]` if set, otherwise a single
    /// [`Self::agent`].
    pub fn all_agents(&self) -> Vec<AgentCfg> {
        if self.agents.is_empty() {
            vec![self.agent.clone()]
        } else {
            self.agents.clone()
        }
    }
    pub fn model(&self) -> &str {
        &self.provider.model
    }
    /// Fallback models in order, read from the `FAMILYCLAW_FALLBACK_MODELS`
    /// env var (comma-separated list, e.g.
    /// `"nvidia/nemotron-3-ultra-550b-a55b,deepseek-ai/deepseek-v4-pro"`).
    /// Empty entries and ones identical to the primary are pruned. Empty list =
    /// no fallbacks (same behavior as before). Layer A: no hardcoded
    /// model names — the per-operator chain comes from the environment.
    pub fn fallback_models(&self) -> Vec<String> {
        std::env::var("FAMILYCLAW_FALLBACK_MODELS")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|m| !m.is_empty() && *m != self.provider.model)
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
    /// Primary + fallback models as a runnable [`ModelConfig`] (same as the serve path).
    pub fn model_config(&self) -> familyclaw_core::ModelConfig {
        let mut cfg = familyclaw_core::ModelConfig::new(self.model().to_string());
        for fb in self.fallback_models() {
            cfg = cfg.with_fallback(fb);
        }
        cfg
    }
    pub fn channel_kind(&self) -> &str {
        &self.channel.kind
    }
    pub fn reply_target(&self) -> &str {
        &self.channel.reply_target
    }
    pub fn discord_webhook_url(&self) -> &str {
        &self.channel.discord.webhook_url
    }
    pub fn discord_channel_id(&self) -> &str {
        &self.channel.discord.channel_id
    }
    pub fn discord_public_key(&self) -> &str {
        &self.channel.discord.public_key
    }
    pub fn discord_bot_token(&self) -> &str {
        &self.channel.discord.bot_token
    }
    /// Operator's Discord user id for the DM gate. 0 = unset → DMs off.
    pub fn discord_owner_id(&self) -> u64 {
        self.channel.discord.owner_id
    }
    pub fn telegram_token(&self) -> &str {
        &self.channel.telegram.token
    }
    pub fn telegram_channel_id(&self) -> &str {
        &self.channel.telegram.channel_id
    }
    /// Optional `POST /inject` bearer token. Empty = no protection
    /// (loopback-only default behavior).
    pub fn gateway_token(&self) -> &str {
        &self.security.gateway_token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIX 3: `FAMILYCLAW_REPLY_TARGET` is the canonical reply-target
    /// environment variable (same name as .env.example, RUNBOOK, and main.rs);
    /// `FAMILYCLAW_CHANNEL_REPLY_TARGET` remains a deprecated alias.
    /// The canonical one wins if both are set — so a user following the docs
    /// gets the expected behavior.
    ///
    /// Env vars are process-wide → all cases run sequentially in one test
    /// (no parallel race with other tests) and are cleaned up at the end.
    #[test]
    fn reply_target_env_canonical_wins_over_deprecated_alias() {
        const CANON: &str = "FAMILYCLAW_REPLY_TARGET";
        const ALIAS: &str = "FAMILYCLAW_CHANNEL_REPLY_TARGET";

        // Starting state: neither set → reply_target stays default-empty.
        std::env::remove_var(CANON);
        std::env::remove_var(ALIAS);
        let mut cfg = FamilyConfig::default();
        cfg.apply_env();
        assert_eq!(cfg.reply_target(), "", "no env -> default empty");

        // Only the deprecated alias set → it is read (backward compatibility).
        std::env::set_var(ALIAS, "legacy-target");
        let mut cfg = FamilyConfig::default();
        cfg.apply_env();
        assert_eq!(
            cfg.reply_target(),
            "legacy-target",
            "alias is read when canonical is absent"
        );

        // Both set → CANONICAL wins.
        std::env::set_var(CANON, "canonical-target");
        let mut cfg = FamilyConfig::default();
        cfg.apply_env();
        assert_eq!(
            cfg.reply_target(),
            "canonical-target",
            "canonical wins over the alias"
        );

        // Only canonical set → it is read.
        std::env::remove_var(ALIAS);
        let mut cfg = FamilyConfig::default();
        cfg.apply_env();
        assert_eq!(
            cfg.reply_target(),
            "canonical-target",
            "canonical is read on its own"
        );

        // Cleanup.
        std::env::remove_var(CANON);
        std::env::remove_var(ALIAS);
    }

    /// `FAMILYCLAW_FALLBACK_MODELS` is parsed as a comma-separated list;
    /// empty entries and ones identical to the primary are pruned; missing
    /// env = empty (backward-compatible: the agent runs on the primary only,
    /// as before). Env is process-wide → all cases run sequentially + cleanup.
    #[test]
    fn fallback_models_parsed_from_env() {
        const VAR: &str = "FAMILYCLAW_FALLBACK_MODELS";
        std::env::remove_var(VAR);

        let mut cfg = FamilyConfig::default();
        cfg.provider.model = "primary/model".to_string();

        // No env → empty list (unchanged).
        assert!(cfg.fallback_models().is_empty(), "no env -> no fallbacks");

        // Comma list; whitespace, empty entries, and primary duplicate are pruned.
        std::env::set_var(VAR, " a/one ,, primary/model , b/two ,");
        assert_eq!(
            cfg.fallback_models(),
            vec!["a/one".to_string(), "b/two".to_string()],
            "trim + pruning of empty entries and the primary"
        );

        std::env::remove_var(VAR);
    }

    /// Operator's `owner_id` loads from TOML as a `DiscordCfg` field.
    #[test]
    fn owner_id_loads_from_toml() {
        let toml = r"
[channel.discord]
owner_id = 123456789
";
        let cfg: FamilyConfig = toml::from_str(toml).expect("parse toml");
        assert_eq!(cfg.discord_owner_id(), 123_456_789);
    }

    /// Missing `owner_id` → default 0 → DMs off (safe default).
    #[test]
    fn missing_owner_id_defaults_to_zero_disabling_dms() {
        let cfg = FamilyConfig::default();
        assert_eq!(
            cfg.discord_owner_id(),
            0,
            "missing owner_id -> 0 -> DMs dropped, never 'all allowed'"
        );
    }

    /// `FAMILYCLAW_OWNER_ID` overrides the TOML value; an invalid value does
    /// NOT override the safe default (keeps the TOML/default value + warns).
    ///
    /// Env vars are process-wide → all cases run sequentially in one test
    /// and cleanup at the end.
    #[test]
    fn owner_id_env_overrides_toml_and_invalid_fails_safe() {
        const ENV: &str = "FAMILYCLAW_OWNER_ID";
        std::env::remove_var(ENV);

        // Valid env → overrides the TOML value.
        let mut cfg: FamilyConfig =
            toml::from_str("[channel.discord]\nowner_id = 111\n").expect("parse toml");
        std::env::set_var(ENV, "222");
        cfg.apply_env();
        assert_eq!(cfg.discord_owner_id(), 222, "env wins over TOML");

        // Invalid env → does NOT override: the TOML value is kept (fail-safe + warning).
        let mut cfg: FamilyConfig =
            toml::from_str("[channel.discord]\nowner_id = 333\n").expect("parse toml");
        std::env::set_var(ENV, "not-a-number");
        cfg.apply_env();
        assert_eq!(
            cfg.discord_owner_id(),
            333,
            "invalid env does not override the safe default -> TOML value is kept"
        );

        // Invalid env without a TOML value → stays at default 0 (DMs off).
        let mut cfg = FamilyConfig::default();
        std::env::set_var(ENV, "xyz");
        cfg.apply_env();
        assert_eq!(
            cfg.discord_owner_id(),
            0,
            "invalid env + no TOML -> 0 -> DMs off (fail-safe)"
        );

        std::env::remove_var(ENV);
    }

    /// FIX 4: the default model must be in provider-prefixed `provider/model`
    /// form, otherwise the resolver interprets the bare name as the provider
    /// name and the agent goes mute. Guards against a regression back to a bare name.
    #[test]
    fn default_provider_model_is_provider_prefixed() {
        let cfg = ProviderCfg::default();
        assert!(
            cfg.model.contains('/'),
            "default model '{}' must be provider/model form",
            cfg.model
        );
    }

    /// `[[agents]]` parses as a TOML list; empty list → only `[agent]`.
    #[test]
    fn agents_table_parses_from_toml() {
        let toml = r#"
[[agents]]
name = "agent_a"

[[agents]]
name = "agent_b"
reply_target = "telegram:99"
"#;
        let cfg: FamilyConfig = toml::from_str(toml).expect("parse");
        let all = cfg.all_agents();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "agent_a");
        assert_eq!(all[1].name, "agent_b");
        assert_eq!(all[1].reply_target, "telegram:99");
    }

    #[test]
    fn empty_agents_list_falls_back_to_single_agent() {
        let cfg = FamilyConfig::default();
        assert_eq!(cfg.all_agents().len(), 1);
        assert_eq!(cfg.all_agents()[0].name, "agent_a");
    }
}
