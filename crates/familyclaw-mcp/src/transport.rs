//! JSON-RPC transports: stdio and HTTP (Layer A).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;

use crate::env::McpTransportConfig;
use crate::error::{McpError, Result};
use crate::redact::redact_command_line;

/// Shared JSON-RPC id counter.
type RpcId = Arc<AtomicU64>;

fn next_id(counter: &RpcId) -> u64 {
    counter.fetch_add(1, Ordering::Relaxed) + 1
}

/// An active transport for a stdio or HTTP connection.
pub enum Transport {
    /// JSON-RPC lines over a child process's stdin/stdout.
    ///
    /// Boxed: `StdioTransport` is considerably larger than `HttpTransport`
    /// (child process + mutexes), so the indirection keeps the enum's
    /// variant sizes balanced.
    Stdio(Box<StdioTransport>),
    /// HTTP POST to the `/mcp` endpoint.
    Http(HttpTransport),
}

impl Transport {
    /// Opens a transport from configuration.
    ///
    /// # Errors
    /// Returns an error if spawning the process, creating the HTTP client,
    /// or the MCP handshake fails.
    pub fn connect(config: &McpTransportConfig, server_name: &str) -> Result<Self> {
        match config {
            McpTransportConfig::Stdio { command, args } => {
                let transport = StdioTransport::spawn(command, args, server_name)?;
                Ok(Self::Stdio(Box::new(transport)))
            }
            McpTransportConfig::Http { url } => {
                let transport = HttpTransport::new(url.clone())?;
                Ok(Self::Http(transport))
            }
        }
    }

    /// Performs a JSON-RPC request and returns the `result` field.
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        match self {
            Self::Stdio(t) => t.request(method, params).await,
            Self::Http(t) => t.request(method, params).await,
        }
    }

    /// MCP `initialize` + `notifications/initialized` handshake.
    pub async fn handshake(&mut self) -> Result<()> {
        let params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "familyclaw-mcp",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });
        let _ = self.request("initialize", params).await?;
        self.notify("notifications/initialized", json!({})).await
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        match self {
            Self::Stdio(t) => t.notify(method, params).await,
            Self::Http(t) => t.notify(method, params).await,
        }
    }
}

/// Stdio JSON-RPC transport (child process).
pub struct StdioTransport {
    _child: Child,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<tokio::process::ChildStdout>>,
    id: RpcId,
}

impl StdioTransport {
    fn spawn(command: &str, args: &[String], server_name: &str) -> Result<Self> {
        let mut cmd_parts = vec![command.to_string()];
        cmd_parts.extend(args.iter().cloned());
        tracing::info!(
            target: "familyclaw::mcp",
            server = %server_name,
            command = %redact_command_line(&cmd_parts),
            "spawning MCP stdio server"
        );

        let mut child = Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| McpError::ProcessSpawn(format!("{command}: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::ProcessSpawn("stdin pipe missing".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::ProcessSpawn("stdout pipe missing".to_string()))?;

        Ok(Self {
            _child: child,
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            id: Arc::new(AtomicU64::new(0)),
        })
    }

    async fn write_line(&self, line: &str) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::TransportSend(e.to_string()))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| McpError::TransportSend(e.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|e| McpError::TransportSend(e.to_string()))?;
        Ok(())
    }

    async fn read_line(&self) -> Result<String> {
        let mut stdout = self.stdout.lock().await;
        let mut line = String::new();
        stdout
            .read_line(&mut line)
            .await
            .map_err(|e| McpError::TransportRecv(e.to_string()))?;
        if line.is_empty() {
            return Err(McpError::TransportRecv(
                "stdio stream closed unexpectedly".to_string(),
            ));
        }
        Ok(line)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let line =
            serde_json::to_string(&msg).map_err(|e| McpError::TransportSend(e.to_string()))?;
        self.write_line(&line).await
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = next_id(&self.id);
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let line =
            serde_json::to_string(&msg).map_err(|e| McpError::TransportSend(e.to_string()))?;
        self.write_line(&line).await?;

        loop {
            let response_line = self.read_line().await?;
            let response: Value = serde_json::from_str(response_line.trim())
                .map_err(|e| McpError::TransportRecv(format!("invalid json: {e}")))?;

            if response.get("method").is_some() && response.get("id").is_none() {
                // Server notification — ignore it and keep waiting for the response.
                continue;
            }

            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }

            if let Some(err) = response.get("error") {
                let message = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown json-rpc error");
                return Err(McpError::JsonRpc(message.to_string()));
            }

            return response
                .get("result")
                .cloned()
                .ok_or_else(|| McpError::Protocol("response missing result".to_string()));
        }
    }
}

/// HTTP JSON-RPC transport (POST `/mcp`).
pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    id: RpcId,
}

impl HttpTransport {
    fn new(url: String) -> Result<Self> {
        #[cfg(not(feature = "http"))]
        {
            let _ = url;
            return Err(McpError::HttpDisabled);
        }

        #[cfg(feature = "http")]
        {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| McpError::ProcessSpawn(e.to_string()))?;
            tracing::info!(
                target: "familyclaw::mcp",
                url = %crate::redact::redact_for_log(&url),
                "configured MCP HTTP transport"
            );
            Ok(Self {
                client,
                url,
                id: Arc::new(AtomicU64::new(0)),
            })
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.post_json(msg).await?;
        Ok(())
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = next_id(&self.id);
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let response = self.post_json(msg).await?;
        if let Some(err) = response.get("error") {
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown json-rpc error");
            return Err(McpError::JsonRpc(message.to_string()));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| McpError::Protocol("response missing result".to_string()))
    }

    async fn post_json(&self, body: Value) -> Result<Value> {
        #[cfg(not(feature = "http"))]
        {
            let _ = body;
            return Err(McpError::HttpDisabled);
        }

        #[cfg(feature = "http")]
        {
            let resp = self
                .client
                .post(&self.url)
                .json(&body)
                .send()
                .await
                .map_err(|e| McpError::TransportSend(e.to_string()))?;
            let status = resp.status();
            let text = resp
                .text()
                .await
                .map_err(|e| McpError::TransportRecv(e.to_string()))?;
            if !status.is_success() {
                return Err(McpError::JsonRpc(format!(
                    "http {status}: {}",
                    crate::redact::redact_for_log(&text)
                )));
            }
            serde_json::from_str(&text)
                .map_err(|e| McpError::TransportRecv(format!("invalid json: {e}")))
        }
    }
}
