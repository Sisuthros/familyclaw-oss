//! Core of the ACP client — spawns a CLI agent and manages JSON message traffic.
//!
//! [`AcpClient`] launches a CLI agent as a subprocess in ACP mode
//! (the `--acp` flag), sends prompts to stdin as JSON, and returns
//! responses as [`AcpResponse`] objects.
//!
//! ## Usage
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

/// An active ACP connection to a CLI agent.
///
/// Owns the subprocess and manages the stdin/stdout connection.
/// Release it with [`shutdown`](Self::shutdown) when the session ends.
#[derive(Debug)]
pub struct AcpClient {
    /// Configuration the agent was spawned with.
    config: AcpAgentConfig,
    /// The subprocess (kept alive for the duration of the session).
    child: Child,
    /// The agent's stdout reader (line-based JSON).
    reader: BufReader<tokio::process::ChildStdout>,
}

impl AcpClient {
    /// Spawns the ACP agent as a subprocess.
    ///
    /// Automatically adds the `--acp` flag to the arguments.
    ///
    /// # Errors
    /// [`AcpError::Spawn`] if the binary cannot be found or the process fails to start.
    pub fn spawn(config: &AcpAgentConfig) -> Result<Self, AcpError> {
        let mut cmd = Command::new(&config.binary);

        // Enable ACP mode
        cmd.arg("--acp");

        // Additional arguments
        for arg in &config.args {
            cmd.arg(arg);
        }

        // Working directory
        if let Some(ref dir) = config.working_dir {
            cmd.current_dir(dir);
        }

        // Stdio: stdin for writing, stdout for reading,
        // stderr is inherited (so debug logs are visible).
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::inherit());

        // Prevent the process from inheriting signals directly
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

    /// Sends a prompt to the agent and returns the response.
    ///
    /// # Errors
    /// [`AcpError::Io`] if the stdin/stdout connection breaks.
    /// [`AcpError::Timeout`] if the agent doesn't respond within the time limit.
    /// [`AcpError::Json`] if the response cannot be parsed.
    pub async fn send(&mut self, request: AcpRequest) -> Result<AcpResponse, AcpError> {
        let stdin = self.child.stdin.as_mut().ok_or_else(|| AcpError::Spawn {
            binary: self.config.binary.clone(),
            reason: "stdin already consumed".to_string(),
        })?;

        // Send the prompt as JSON
        let json = serde_json::to_string(&request)?;
        let mut line = json;
        line.push('\n');
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;

        tracing::debug!(agent = %self.config.name, "ACP prompt sent");

        // Read the response (a single JSON line)
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

    /// Reads a single JSON response from the agent's stdout.
    async fn read_response(&mut self) -> Result<AcpResponse, AcpError> {
        let mut line = String::new();
        self.reader.read_line(&mut line).await?;

        if line.is_empty() {
            // The agent exited — check the exit code
            let status = self.child.wait().await?;
            return Err(AcpError::Crash {
                agent: self.config.name.clone(),
                code: status.code().unwrap_or(-1),
            });
        }

        let trimmed = line.trim();
        serde_json::from_str(trimmed).map_err(AcpError::from)
    }

    /// Shuts the agent down cleanly.
    ///
    /// Sends a shutdown message and waits for the process to exit.
    pub async fn shutdown(mut self) -> Result<(), AcpError> {
        // Attempt a clean shutdown
        if let Some(ref mut stdin) = self.child.stdin {
            let _ = stdin.write_all(b"{\"shutdown\": true}\n").await;
            let _ = stdin.flush().await;
        }

        // Give it a moment to clean up, then force-kill
        let _ = timeout(std::time::Duration::from_secs(5), self.child.wait()).await;

        if let Err(e) = self.child.kill().await {
            // The process may already be dead — that's fine
            tracing::debug!(agent = %self.config.name, "kill result: {e:?}");
        }

        tracing::info!(agent = %self.config.name, "ACP agent shut down");
        Ok(())
    }

    /// Returns the agent's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Returns the process PID (if alive).
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }
}

// AcpClient is NOT Send/Sync because it owns the subprocess —
// this is intentional. The agent is owned by a single thread.
//
// To share an agent across multiple threads, use `Arc<Mutex<AcpClient>>`
// or an `Actor` pattern (Ractor).
