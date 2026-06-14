//! ACP-clientin ydin — spawnaa CLI-agentin ja hallinnoi JSON-viestiliikennettä.
//!
//! [`AcpClient`] käynnistää CLI-agentin aliprosessina ACP-moodissa
//! (`--acp`-lippu), lähettää promptit stdin:iin JSON-muodossa ja palauttaa
//! vastaukset [`AcpResponse`]-olioina.
//!
//! ## Käyttö
//! ```ignore
//! use familyclaw_acp::{AcpClient, AcpAgentConfig, AcpRequest};
//!
//! let config = AcpAgentConfig::new("claude");
//! let mut client = AcpClient::spawn(&config).await?;
//! let response = client.send(AcpRequest::new("what is 2+2?")).await?;
//! println!("{}", response.content);
//! client.shutdown().await?;
//! ```

use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::timeout;

use crate::config::AcpAgentConfig;
use crate::error::AcpError;
use crate::message::{AcpRequest, AcpResponse};

/// Aktiivinen ACP-yhteys CLI-agenttiin.
///
/// Omistaa aliprosessin ja hallinnoi stdin/stdout-yhteyttä.
/// Vapauta [`shutdown`](Self::shutdown):lla kun sessio päättyy.
#[derive(Debug)]
pub struct AcpClient {
    /// Konfiguraatio jolla agentti spawnattiin.
    config: AcpAgentConfig,
    /// Aliprosessi (pidetään elossa koko session ajan).
    child: Child,
    /// Agentin stdout-lukija (rivipohjainen JSON).
    reader: BufReader<tokio::process::ChildStdout>,
}

impl AcpClient {
    /// Spawnaa ACP-agentin aliprosessina.
    ///
    /// Lisää automaattisesti `--acp`-lipun argumentteihin.
    ///
    /// # Errors
    /// [`AcpError::Spawn`] jos binääriä ei löydy tai prosessi ei käynnisty.
    pub fn spawn(config: &AcpAgentConfig) -> Result<Self, AcpError> {
        let mut cmd = Command::new(&config.binary);

        // ACP-moodi päälle
        cmd.arg("--acp");

        // Lisäargumentit
        for arg in &config.args {
            cmd.arg(arg);
        }

        // Työhakemisto
        if let Some(ref dir) = config.working_dir {
            cmd.current_dir(dir);
        }

        // Stdio: stdin kirjoitusta varten, stdout lukemista varten,
        // stderr peritään (debug-lokit näkyviin).
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::inherit());

        // Estä prosessia perimästä signaaleja suoraan
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| AcpError::Spawn {
            binary: config.binary.clone(),
            reason: format!("spawn failed: {e}"),
        })?;

        let stdout = child.stdout.take().ok_or_else(|| AcpError::Spawn {
            binary: config.binary.clone(),
            reason: "no stdout pipe".to_string(),
        })?;

        let reader = BufReader::new(stdout);

        tracing::info!(
            agent = %config.name,
            binary = %config.binary.display(),
            pid = child.id().unwrap_or(0),
            "ACP agent spawned"
        );

        Ok(Self {
            config: config.clone(),
            child,
            reader,
        })
    }

    /// Lähettää promptin agentille ja palauttaa vastauksen.
    ///
    /// # Errors
    /// [`AcpError::Io`] jos stdin/stdout-yhteys katkeaa.
    /// [`AcpError::Timeout`] jos agentti ei vastaa aikarajan puitteissa.
    /// [`AcpError::Json`] jos vastausta ei voi parsia.
    pub async fn send(&mut self, request: AcpRequest) -> Result<AcpResponse, AcpError> {
        let stdin = self.child.stdin.as_mut().ok_or_else(|| AcpError::Spawn {
            binary: self.config.binary.clone(),
            reason: "stdin already consumed".to_string(),
        })?;

        // Lähetä prompt JSON-muodossa
        let json = serde_json::to_string(&request)?;
        let mut line = json;
        line.push('\n');
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;

        tracing::debug!(agent = %self.config.name, "ACP prompt sent");

        // Lue vastaus (yksi JSON-rivi)
        let response = timeout(
            std::time::Duration::from_secs(self.config.timeout_secs),
            self.read_response(),
        )
        .await
        .map_err(|_| AcpError::Timeout {
            agent: self.config.name.clone(),
            secs: self.config.timeout_secs,
        })??;

        tracing::debug!(
            agent = %self.config.name,
            chars = response.content.len(),
            tools = response.tool_calls.len(),
            "ACP response received"
        );

        Ok(response)
    }

    /// Lukee yhden JSON-vastauksen agentin stdout:sta.
    async fn read_response(&mut self) -> Result<AcpResponse, AcpError> {
        let mut line = String::new();
        self.reader.read_line(&mut line).await?;

        if line.is_empty() {
            // Agentti sammui — tarkista exit-koodi
            let status = self.child.wait().await?;
            return Err(AcpError::Crash {
                agent: self.config.name.clone(),
                code: status.code().unwrap_or(-1),
            });
        }

        let trimmed = line.trim();
        serde_json::from_str(trimmed).map_err(AcpError::from)
    }

    /// Samuttaa agentin siististi.
    ///
    /// Lähettää shutdown-viestin ja odottaa prosessin päättymistä.
    pub async fn shutdown(mut self) -> Result<(), AcpError> {
        // Yritä siisti sammutus
        if let Some(ref mut stdin) = self.child.stdin {
            let _ = stdin.write_all(b"{\"shutdown\": true}\n").await;
            let _ = stdin.flush().await;
        }

        // Anna hetki aikaa siivota, sitten force-kill
        let _ = timeout(std::time::Duration::from_secs(5), self.child.wait()).await;

        if let Err(e) = self.child.kill().await {
            // Prosessi saattaa olla jo kuollut — ok
            tracing::debug!(agent = %self.config.name, "kill result: {e:?}");
        }

        tracing::info!(agent = %self.config.name, "ACP agent shut down");
        Ok(())
    }

    /// Palauttaa agentin nimen.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Palauttaa prosessin PID:n (jos elossa).
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }
}

// AcpClient EI ole Send/Sync koska se omistaa aliprosessin —
// tämä on tarkoituksellista. Agentti on yhden säikeen omistuksessa.
//
// Jos halutaan jakaa agentti usealle säikeelle, käytä `Arc<Mutex<AcpClient>>`:ia
// tai `Actor`-mallia (Ractor).
