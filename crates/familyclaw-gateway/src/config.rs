//! TOML-pohjainen konfiguraatio (KERROS B).
//! Lukee `~/.config/familyclaw/familyclaw.toml` + env-ylikirjoitukset.

use familyclaw_core::FamilyClawError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const CONFIG_FILE_NAME: &str = "familyclaw.toml";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
#[derive(Default)]
pub struct FamilyConfig {
    pub agent: AgentCfg,
    pub channel: ChannelCfg,
    pub provider: ProviderCfg,
    pub memory: MemoryCfg,
    pub security: SecurityCfg,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentCfg {
    pub name: String,
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
}

// Defaults
impl Default for AgentCfg {
    fn default() -> Self {
        Self {
            name: "agent_a".into(),
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
        }
    }
}
impl Default for ProviderCfg {
    fn default() -> Self {
        Self {
            kind: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
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

    fn find_path() -> PathBuf {
        if let Ok(p) = std::env::var("FAMILYCLAW_CONFIG") {
            return PathBuf::from(p);
        }
        for b in [
            std::env::var("XDG_CONFIG_HOME").ok(),
            std::env::var("HOME").map(|h| format!("{h}/.config")).ok(),
        ].into_iter().flatten() {
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
        if let Ok(v) = std::env::var("FAMILYCLAW_CHANNEL_REPLY_TARGET") {
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
        if let Ok(v) = std::env::var("FAMILYCLAW_MEMORY_RETENTION_HOURS") {
            if let Ok(n) = v.parse() {
                self.memory.retention_hours = n;
            }
        }
    }
}

// Accessor helpers
impl FamilyConfig {
    pub fn agent_name(&self) -> &str {
        &self.agent.name
    }
    pub fn model(&self) -> &str {
        &self.provider.model
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
    pub fn telegram_token(&self) -> &str {
        &self.channel.telegram.token
    }
    pub fn telegram_channel_id(&self) -> &str {
        &self.channel.telegram.channel_id
    }
}
