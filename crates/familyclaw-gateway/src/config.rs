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
    /// Valinnainen moniagentti-lista (`[[agents]]` TOML:ssa). Tyhjä → käytetään
    /// vain [`FamilyConfig::agent`]-kenttää (taaksepäin-yhteensopiva yksi agentti).
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
    /// Valinnainen per-agentti reply-kohde (ylikirjoittaa kanavan oletuksen).
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
    /// Discord bot token. Jos asetettu, gateway käyttää kaksisuuntaista
    /// serenity-bot-yhteyttä (kuuntelee + postaa) webhook-postauksen sijaan.
    pub bot_token: String,
    /// Huoltajan/operaattorin Discord-user-id. Vain tämä id saa `DMata` agenttia
    /// (kahdenkeskinen keskustelu). 0 = ei asetettu → DM:t pudotetaan kaikilta
    /// (turvallinen oletus, ei koskaan "kaikki DM:t sallittu"). TOML-kentästä tai
    /// env-ylikirjoituksesta `FAMILYCLAW_OWNER_ID`.
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
    /// Valinnainen bearer-token, jolla `POST /inject` suojataan. Tyhjä = ei
    /// tokenia → loopback-only-oletuskäytös (avoin). Asetettuna `/inject`
    /// vaatii `Authorization: Bearer <token>` ja hylkää väärät 401:llä.
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
            // 0 = ei huoltajaa asetettu → DM:t pois (turvallinen oletus).
            owner_id: 0,
        }
    }
}
impl Default for ProviderCfg {
    fn default() -> Self {
        Self {
            kind: "openai".into(),
            // Provider-prefixed (`provider/model`) muoto: resolveri vaatii sen,
            // muuten bare-nimi tulkitaan provider-nimeksi → ei ratkea → agentti
            // jää mykäksi (ei tekstivastauksia). Ks. build_llm_chain.
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
        // Reply-kohde: kanoninen `FAMILYCLAW_REPLY_TARGET` (sama nimi kuin
        // .env.example, docs/RUNBOOK_WINDOWS.md ja main.rs:n REPLY_TARGET_ENV).
        // `FAMILYCLAW_CHANNEL_REPLY_TARGET` säilytetään vanhentuneena aliaksena
        // taaksepäin-yhteensopivuudelle — luetaan VAIN jos kanonista ei ole
        // asetettu (kanoninen voittaa, jos molemmat on asetettu).
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
        // Huoltajan Discord-user-id DM-portille. Virheellinen arvo EI ohita
        // turvallista oletusta: varoitetaan ja säilytetään TOML-/default-arvo
        // (ei koskaan "kaikki DM:t sallittu"). Tyhjä string parsitaan myös
        // virheelliseksi → varoitus, mutta tyhjä env-arvo on harvinainen.
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
    // Osa julkista accessor-pintaa `model()`/`all_agents()`:n rinnalla.
    // Nykyinen koodi lukee `agent.name`-kentän suoraan, joten metodia ei
    // vielä kutsuta — säilytetään API-symmetrian vuoksi.
    #[allow(dead_code)]
    pub fn agent_name(&self) -> &str {
        &self.agent.name
    }

    /// Palauttaa kaikki serve-agentit: `[[agents]]` jos asetettu, muuten yksi
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
    /// Varamallit järjestyksessä, luettuna `FAMILYCLAW_FALLBACK_MODELS`-env-
    /// muuttujasta (pilkulla erotettu lista, esim.
    /// `"nvidia/nemotron-3-ultra-550b-a55b,deepseek-ai/deepseek-v4-pro"`).
    /// Tyhjät ja primaryn kanssa identtiset karsitaan. Tyhjä lista = ei
    /// fallbackeja (sama käytös kuin ennen). KERROS A: ei kovakoodattuja
    /// mallinimiä — perheenjäsenkohtainen ketju tulee ympäristöstä.
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
    /// Primary + varamallit ajettavaksi [`ModelConfig`]:ksi (sama kuin serve-polku).
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
    /// Huoltajan Discord-user-id DM-portille. 0 = ei asetettu → DM:t pois.
    pub fn discord_owner_id(&self) -> u64 {
        self.channel.discord.owner_id
    }
    pub fn telegram_token(&self) -> &str {
        &self.channel.telegram.token
    }
    pub fn telegram_channel_id(&self) -> &str {
        &self.channel.telegram.channel_id
    }
    /// Valinnainen `POST /inject`-bearer-token. Tyhjä = ei suojausta
    /// (loopback-only-oletuskäytös).
    pub fn gateway_token(&self) -> &str {
        &self.security.gateway_token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIX 3: `FAMILYCLAW_REPLY_TARGET` on kanoninen reply-kohteen
    /// ympäristömuuttuja (sama nimi kuin .env.example, RUNBOOK ja main.rs);
    /// `FAMILYCLAW_CHANNEL_REPLY_TARGET` jää vanhentuneeksi aliakseksi.
    /// Kanoninen voittaa, jos molemmat on asetettu — niin docs-ohjeita
    /// seuraava käyttäjä saa odotetun käytöksen.
    ///
    /// Env-muuttujat ovat prosessin laajuisia → ajetaan kaikki tapaukset
    /// peräkkäin yhdessä testissä (ei rinnakkaiskilpailua muiden testien
    /// kanssa) ja siivotaan lopuksi.
    #[test]
    fn reply_target_env_canonical_wins_over_deprecated_alias() {
        const CANON: &str = "FAMILYCLAW_REPLY_TARGET";
        const ALIAS: &str = "FAMILYCLAW_CHANNEL_REPLY_TARGET";

        // Lähtötilanne: kumpikaan ei asetettu → reply_target pysyy default-tyhjänä.
        std::env::remove_var(CANON);
        std::env::remove_var(ALIAS);
        let mut cfg = FamilyConfig::default();
        cfg.apply_env();
        assert_eq!(cfg.reply_target(), "", "ei env → default tyhjä");

        // Vain vanhentunut alias → luetaan (taaksepäin-yhteensopivuus).
        std::env::set_var(ALIAS, "legacy-target");
        let mut cfg = FamilyConfig::default();
        cfg.apply_env();
        assert_eq!(
            cfg.reply_target(),
            "legacy-target",
            "alias luetaan kun ei kanonista"
        );

        // Molemmat asetettu → KANONINEN voittaa.
        std::env::set_var(CANON, "canonical-target");
        let mut cfg = FamilyConfig::default();
        cfg.apply_env();
        assert_eq!(
            cfg.reply_target(),
            "canonical-target",
            "kanoninen voittaa aliaksen"
        );

        // Vain kanoninen → luetaan.
        std::env::remove_var(ALIAS);
        let mut cfg = FamilyConfig::default();
        cfg.apply_env();
        assert_eq!(
            cfg.reply_target(),
            "canonical-target",
            "kanoninen luetaan yksinään"
        );

        // Siivous.
        std::env::remove_var(CANON);
        std::env::remove_var(ALIAS);
    }

    /// `FAMILYCLAW_FALLBACK_MODELS` parsitaan pilkulla erotetuksi listaksi;
    /// tyhjät ja primaryn kanssa identtiset karsitaan; puuttuva env = tyhjä
    /// (taaksepäin-yhteensopiva: agentti ajaa vain primaryllä kuten ennen).
    /// Env on prosessin laajuinen → kaikki tapaukset peräkkäin + siivous.
    #[test]
    fn fallback_models_parsed_from_env() {
        const VAR: &str = "FAMILYCLAW_FALLBACK_MODELS";
        std::env::remove_var(VAR);

        let mut cfg = FamilyConfig::default();
        cfg.provider.model = "primary/model".to_string();

        // Ei env → tyhjä lista (ennallaan).
        assert!(cfg.fallback_models().is_empty(), "ei env → ei fallbackeja");

        // Pilkkulista, välilyönnit + tyhjät + primary-duplikaatti karsitaan.
        std::env::set_var(VAR, " a/one ,, primary/model , b/two ,");
        assert_eq!(
            cfg.fallback_models(),
            vec!["a/one".to_string(), "b/two".to_string()],
            "trim + tyhjien + primaryn karsinta"
        );

        std::env::remove_var(VAR);
    }

    /// Huoltajan `owner_id` latautuu TOML:sta DiscordCfg-kenttänä.
    #[test]
    fn owner_id_loads_from_toml() {
        let toml = r"
[channel.discord]
owner_id = 123456789
";
        let cfg: FamilyConfig = toml::from_str(toml).expect("parse toml");
        assert_eq!(cfg.discord_owner_id(), 123_456_789);
    }

    /// Puuttuva `owner_id` → oletus 0 → DM:t pois (turvallinen oletus).
    #[test]
    fn missing_owner_id_defaults_to_zero_disabling_dms() {
        let cfg = FamilyConfig::default();
        assert_eq!(
            cfg.discord_owner_id(),
            0,
            "puuttuva owner_id → 0 → DM:t pudotetaan, ei koskaan 'kaikki sallittu'"
        );
    }

    /// `FAMILYCLAW_OWNER_ID` ylikirjoittaa TOML-arvon; virheellinen arvo
    /// EI ohita turvallista oletusta (säilyttää TOML-/default-arvon + varoittaa).
    ///
    /// Env-muuttujat ovat prosessin laajuisia → kaikki tapaukset peräkkäin
    /// yhdessä testissä ja siivous lopuksi.
    #[test]
    fn owner_id_env_overrides_toml_and_invalid_fails_safe() {
        const ENV: &str = "FAMILYCLAW_OWNER_ID";
        std::env::remove_var(ENV);

        // Validi env → ylikirjoittaa TOML-arvon.
        let mut cfg: FamilyConfig =
            toml::from_str("[channel.discord]\nowner_id = 111\n").expect("parse toml");
        std::env::set_var(ENV, "222");
        cfg.apply_env();
        assert_eq!(cfg.discord_owner_id(), 222, "env voittaa TOML:n");

        // Virheellinen env → EI ohita: TOML-arvo säilyy (fail-safe + varoitus).
        let mut cfg: FamilyConfig =
            toml::from_str("[channel.discord]\nowner_id = 333\n").expect("parse toml");
        std::env::set_var(ENV, "not-a-number");
        cfg.apply_env();
        assert_eq!(
            cfg.discord_owner_id(),
            333,
            "virheellinen env ei ohita turvallista oletusta → TOML-arvo säilyy"
        );

        // Virheellinen env ilman TOML-arvoa → pysyy default 0 (DM:t pois).
        let mut cfg = FamilyConfig::default();
        std::env::set_var(ENV, "xyz");
        cfg.apply_env();
        assert_eq!(
            cfg.discord_owner_id(),
            0,
            "virheellinen env + ei TOML → 0 → DM:t pois (fail-safe)"
        );

        std::env::remove_var(ENV);
    }

    /// FIX 4: oletusmallin on oltava provider-prefixed `provider/model`
    /// muodossa, muuten resolveri tulkitsee bare-nimen provider-nimeksi ja
    /// agentti jää mykäksi. Suojaa regressiolta takaisin bare-nimeen.
    #[test]
    fn default_provider_model_is_provider_prefixed() {
        let cfg = ProviderCfg::default();
        assert!(
            cfg.model.contains('/'),
            "default model '{}' must be provider/model form",
            cfg.model
        );
    }

    /// `[[agents]]` parsitaan TOML-listana; tyhjä lista → vain `[agent]`.
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
