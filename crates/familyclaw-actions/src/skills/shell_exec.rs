//! Flagship skill: allowlisted command-line execution (Layer A).
//!
//! [`ShellExec`] executes a single command-line command with a restricted working
//! directory. The Hermes-style **hard block** cannot be bypassed by any mode (`manual` /
//! `smart` / `off`). Three modes ([`ShellMode`]) are determined by the environment
//! (`FAMILYCLAW_SHELL_MODE`):
//!
//! - **manual** (default): [`ApprovalPolicy::AlwaysRequireApproval`] — all
//!   allowed commands require approval.
//! - **smart**: safe read-only commands (`ls`, `dir`, `echo`, `pwd`, …)
//!   run automatically ([`ActionRisk::ReadOnly`] +
//!   [`ApprovalPolicy::AutoIfReadOnly`]); others are rejected or require approval.
//! - **off**: rejects all commands (the skill is registered but does not execute).
//!
//! The working directory is restricted via [`ShellExecConfig::cwd_allowlist`]
//! (`FAMILYCLAW_SHELL_CWD_ALLOWLIST`, semicolon-separated). An empty allowlist =
//! fail-closed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{ActionError, Result};
use crate::executor::{ActionExecutor, ActionRequest, ActionResult, ActionStatus};
use crate::ids::SkillId;
use crate::manifest::SkillManifest;
use crate::policy::{ActionRisk, ApprovalPolicy, SkillPermission};

use super::Skill;

/// Fixed identifier of the skill.
const SKILL_UUID: uuid::Uuid = uuid::uuid!("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");

/// Default timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Maximum output length in the evidence record (bytes per stream).
const OUTPUT_MAX_BYTES: usize = 4_096;

/// Environment variable: `manual` | `smart` | `off`.
pub const ENV_SHELL_MODE: &str = "FAMILYCLAW_SHELL_MODE";

/// Environment variable: semicolon-separated working-directory allowlist.
pub const ENV_CWD_ALLOWLIST: &str = "FAMILYCLAW_SHELL_CWD_ALLOWLIST";

/// Execution mode (`FAMILYCLAW_SHELL_MODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShellMode {
    /// All allowed commands require approval (default).
    #[default]
    Manual,
    /// Only safe read-only commands run automatically.
    Smart,
    /// Rejects all commands.
    Off,
}

impl ShellMode {
    /// Parses the mode from a string (case-insensitive).
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "smart" => Self::Smart,
            "off" | "disabled" | "none" => Self::Off,
            _ => Self::Manual,
        }
    }
}

/// Configuration: mode, working-directory allowlist and timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellExecConfig {
    mode: ShellMode,
    cwd_allowlist: Vec<PathBuf>,
    timeout_secs: u64,
}

impl Default for ShellExecConfig {
    fn default() -> Self {
        Self {
            mode: ShellMode::Manual,
            cwd_allowlist: Vec::new(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        }
    }
}

impl ShellExecConfig {
    /// Creates the default configuration (manual, empty allowlist = fail-closed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads the configuration from the environment.
    #[must_use]
    pub fn from_env() -> Self {
        let mode = std::env::var(ENV_SHELL_MODE)
            .ok()
            .map(|v| ShellMode::parse(&v))
            .unwrap_or_default();
        let cwd_allowlist = std::env::var(ENV_CWD_ALLOWLIST)
            .ok()
            .map(|raw| {
                raw.split(';')
                    .filter(|p| !p.trim().is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            mode,
            cwd_allowlist,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        }
    }

    /// Sets the mode (builder).
    #[must_use]
    pub fn mode(mut self, mode: ShellMode) -> Self {
        self.mode = mode;
        self
    }

    /// Adds an allowed working-directory root (builder).
    #[must_use]
    pub fn allow_cwd(mut self, root: impl Into<PathBuf>) -> Self {
        self.cwd_allowlist.push(root.into());
        self
    }

    /// Sets the timeout in seconds (builder).
    #[must_use]
    pub fn timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// The current mode.
    #[must_use]
    pub const fn shell_mode(&self) -> ShellMode {
        self.mode
    }

    /// Number of allowed working-directory roots.
    #[must_use]
    pub fn cwd_root_count(&self) -> usize {
        self.cwd_allowlist.len()
    }
}

/// Input: the command to execute and an optional working directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellExecInput {
    /// Command-line command (single line).
    pub command: String,
    /// Optional working directory (will be canonicalized; must remain under the allowlist).
    #[serde(default)]
    pub cwd: Option<String>,
    /// Added 2026-07-31 (per design doc): optional idempotency key.
    /// When present, replay of the same key returns the cached response.
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

/// Result: exit code and truncated streams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellExecOutput {
    /// Process exit code (`-1` on timeout).
    pub exit_code: i32,
    /// Truncated stdout (at most 4 KiB).
    pub stdout_summary: String,
    /// Truncated stderr (at most 4 KiB).
    pub stderr_summary: String,
    /// True if the command exceeded the timeout.
    pub timed_out: bool,
}

/// Allowlisted command-line execution with a Hermes-style hard blocklist.
#[derive(Debug, Clone)]
pub struct ShellExec {
    config: ShellExecConfig,
}

impl Default for ShellExec {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellExec {
    /// Creates the skill with the default configuration (manual, fail-closed allowlist).
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: ShellExecConfig::new(),
        }
    }

    /// Creates the skill with the given configuration.
    #[must_use]
    pub fn with_config(config: ShellExecConfig) -> Self {
        Self { config }
    }

    /// Fixed identifier of the skill.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(SKILL_UUID)
    }

    /// Hermes-style hard block — not bypassable by any mode.
    ///
    /// Returns the block reason if the command is forbidden.
    #[must_use]
    pub fn detect_hardline_block(command: &str) -> Option<&'static str> {
        let normalized = normalize_for_detection(command);
        if normalized.is_empty() {
            return Some("empty command");
        }

        if is_fork_bomb(&normalized) {
            return Some("fork bomb");
        }
        if is_rm_rf_catastrophic(&normalized) {
            return Some("recursive delete of protected path");
        }
        if contains_word(&normalized, "mkfs") {
            return Some("format filesystem (mkfs)");
        }
        if is_dd_to_block_device(&normalized) {
            return Some("dd to raw block device");
        }
        if is_redirect_to_block_device(&normalized) {
            return Some("redirect to raw block device");
        }
        if is_kill_all(&normalized) {
            return Some("kill all processes");
        }
        if is_system_power_command(&normalized) {
            return Some("system shutdown/reboot");
        }
        if is_windows_format(&normalized) {
            return Some("format disk");
        }
        if is_pipe_to_shell_at_root(&normalized) {
            return Some("pipe to shell at root level");
        }

        None
    }

    fn resolve_cwd(&self, requested: Option<&str>) -> Result<PathBuf> {
        let roots = canonicalize_roots(&self.config.cwd_allowlist);
        if roots.is_empty() {
            return Err(ActionError::PolicyDenied(
                "no allowed working directories — all commands are rejected (fail-closed)"
                    .to_string(),
            ));
        }

        let cwd = match requested {
            Some(path) => PathBuf::from(path),
            None => std::env::current_dir().map_err(|e| {
                ActionError::PolicyDenied(format!("cannot resolve working directory: {e}"))
            })?,
        };

        let canonical = std::fs::canonicalize(&cwd).map_err(|e| {
            ActionError::PolicyDenied(format!(
                "cannot canonicalize working directory (rejected): {e}"
            ))
        })?;

        if path_is_under_any(&canonical, &roots) {
            Ok(canonical)
        } else {
            Err(ActionError::PolicyDenied(
                "working directory is outside the allowlist (rejected)".to_string(),
            ))
        }
    }

    fn gate_command(&self, command: &str) -> Result<()> {
        if self.config.mode == ShellMode::Off {
            return Err(ActionError::PolicyDenied(
                "shell_exec is turned off (FAMILYCLAW_SHELL_MODE=off)".to_string(),
            ));
        }

        if let Some(reason) = Self::detect_hardline_block(command) {
            return Err(ActionError::PolicyDenied(format!(
                "hardline blocklist: {reason} (not bypassable)"
            )));
        }

        if self.config.mode == ShellMode::Smart {
            // SECURITY FIX 2026-07-09 (audit finding [3], was LIVE in agent production):
            // is_safe_readonly previously validated only the command name, not the arguments —
            // e.g. `cat /etc/passwd` / `head ~/.ssh/id_rsa` ran without approval
            // because cat/head were on the allowed list, even though the cwd allowlist only
            // restricts the working directory. Now commands that read files (cat/head/tail/type/ls/dir/gci)
            // may only receive file arguments from inside the allowlist.
            let roots = canonicalize_roots(&self.config.cwd_allowlist);
            if !is_safe_readonly_command(command, &roots) {
                return Err(ActionError::PolicyDenied(
                    "in smart mode only safe read-only commands inside the allowlist are auto-allowed"
                        .to_string(),
                ));
            }
        }

        Ok(())
    }

    async fn run_command(&self, command: &str, cwd: &Path) -> Result<ShellExecOutput> {
        let timeout = Duration::from_secs(self.config.timeout_secs);

        let mut cmd = build_shell_command(command);
        cmd.current_dir(cwd);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        let child = cmd
            .spawn()
            .map_err(|e| ActionError::ExecutionFailed(format!("could not start command: {e}")))?;

        let wait_result = tokio::time::timeout(timeout, child.wait_with_output()).await;

        match wait_result {
            Err(_) => Ok(ShellExecOutput {
                exit_code: -1,
                stdout_summary: String::new(),
                stderr_summary: "command timed out".to_string(),
                timed_out: true,
            }),
            Ok(Err(e)) => Err(ActionError::ExecutionFailed(format!(
                "command execution failed: {e}"
            ))),
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                Ok(ShellExecOutput {
                    exit_code,
                    stdout_summary: summarize_bytes(&output.stdout),
                    stderr_summary: summarize_bytes(&output.stderr),
                    timed_out: false,
                })
            }
        }
    }
}

fn build_shell_command(command: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut cmd = tokio::process::Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    }
}

fn canonicalize_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .collect()
}

fn path_is_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Normalizes the command for detection purposes (Hermes-style de-obfuscation).
fn normalize_for_detection(command: &str) -> String {
    let mut s = command.to_string();
    // Unicode NFKC (simplified: only ascii-lowercase + whitespace).
    s = s.to_ascii_lowercase();
    // Remove backslash escapes: r\m -> rm
    s = s.replace('\\', "");
    // Empty quotes: r''m -> rm
    s = s.replace("''", "");
    s = s.replace("\"\"", "");
    // Collapse multiple whitespace into one.
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
}

fn contains_word(haystack: &str, word: &str) -> bool {
    haystack.split_whitespace().any(|token| {
        token
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/')
            .ends_with(word)
            || token == word
    }) || haystack.contains(word)
}

fn is_fork_bomb(s: &str) -> bool {
    s.contains(":(){") && s.contains(":|:") && s.contains("};:") || s.contains(":() { :|:& };:")
}

fn is_rm_rf_catastrophic(s: &str) -> bool {
    for segment in split_command_segments(s) {
        let tokens: Vec<&str> = segment.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        let rm_idx = tokens.iter().position(|t| command_basename(t) == "rm");
        if let Some(idx) = rm_idx {
            let flags_and_target: Vec<&str> = tokens[idx + 1..].to_vec();
            let has_recursive = flags_and_target
                .iter()
                .any(|t| t.starts_with('-') && (t.contains('r') || *t == "--recursive"));
            let has_force = flags_and_target
                .iter()
                .any(|t| t.starts_with('-') && (t.contains('f') || *t == "--force"));
            if has_recursive || has_force {
                for target in &flags_and_target {
                    if target.starts_with('-') {
                        continue;
                    }
                    if is_catastrophic_rm_target(target) {
                        return true;
                    }
                }
            }
            // rm -r without -f still dangerous for /
            for target in &flags_and_target {
                if !target.starts_with('-') && is_catastrophic_rm_target(target) {
                    return true;
                }
            }
        }
    }
    false
}

fn is_catastrophic_rm_target(target: &str) -> bool {
    matches!(
        target,
        "/" | "/*"
            | "/home"
            | "/home/*"
            | "/root"
            | "/root/*"
            | "/etc"
            | "/etc/*"
            | "/usr"
            | "/usr/*"
            | "/var"
            | "/var/*"
            | "/bin"
            | "/bin/*"
            | "/sbin"
            | "/sbin/*"
            | "/boot"
            | "/boot/*"
            | "/lib"
            | "/lib/*"
            | "~"
            | "~/*"
            | "$home"
            | "$home/*"
    ) || target.starts_with("/dev/sd")
        || target.starts_with("/dev/nvme")
        || target.starts_with("/dev/hd")
        || is_named_user_home_root(target)
}

/// Hard-block a NAMED user-home root and its immediate contents — the gap the
/// literal `matches!` list missed (e.g. `rm -rf /home/operator`, `/Users/operator`).
/// Wiping a whole user home is exactly the reported destructive-agent incident.
/// We block the home root and its top-level sweep (`/home/operator`, `/home/operator/*`)
/// but deliberately allow deeper, more specific paths (`/home/operator/project/build`)
/// through to the normal approval gate — those are ordinary, intentional deletes.
fn is_named_user_home_root(target: &str) -> bool {
    // Normalize a trailing "/*" (whole-directory sweep) to the bare dir so
    // `/home/operator` and `/home/operator/*` are treated identically.
    let bare = target.strip_suffix("/*").unwrap_or(target);
    let bare = bare.strip_suffix('/').unwrap_or(bare);
    // NOTE: `target` reaches here already lowercased by normalize_for_detection,
    // so prefixes must be lowercase (`/users/`, not `/Users/`) to match.
    for prefix in ["/home/", "/users/", "/root/", "/export/home/"] {
        if let Some(rest) = bare.strip_prefix(prefix) {
            // rest is the user segment; block iff it is a single non-empty
            // component (the home root itself), not a deeper subpath.
            if !rest.is_empty() && !rest.contains('/') {
                return true;
            }
        }
    }
    false
}

fn is_dd_to_block_device(s: &str) -> bool {
    s.contains("dd") && {
        let lower = s.to_ascii_lowercase();
        lower.contains("of=/dev/sd")
            || lower.contains("of=/dev/nvme")
            || lower.contains("of=/dev/hd")
            || lower.contains("of=/dev/mmcblk")
            || lower.contains("of=/dev/vd")
            || lower.contains("of=/dev/xvd")
    }
}

fn is_redirect_to_block_device(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains(">/dev/sd")
        || lower.contains("> /dev/sd")
        || lower.contains(">/dev/nvme")
        || lower.contains("> /dev/nvme")
        || lower.contains(">/dev/hd")
        || lower.contains("> /dev/hd")
}

fn is_kill_all(s: &str) -> bool {
    for segment in split_command_segments(s) {
        let tokens: Vec<&str> = segment.split_whitespace().collect();
        if let Some(pos) = tokens.iter().position(|t| command_basename(t) == "kill") {
            let args = &tokens[pos + 1..];
            if args
                .iter()
                .any(|a| *a == "-1" || *a == "-9" && args.contains(&"-1"))
            {
                return true;
            }
            if args.windows(2).any(|w| w[0] == "-9" && w[1] == "-1") {
                return true;
            }
        }
        if tokens.iter().any(|t| command_basename(t) == "killall")
            && tokens
                .iter()
                .any(|t| *t == "-9" || *t == "-kill" || *t == "kill")
        {
            return true;
        }
    }
    false
}

fn is_system_power_command(s: &str) -> bool {
    const POWER_CMDS: &[&str] = &[
        "shutdown",
        "reboot",
        "halt",
        "poweroff",
        "telinit",
        "systemctl",
    ];
    for segment in split_command_segments(s) {
        let tokens: Vec<&str> = segment.split_whitespace().collect();
        for (i, token) in tokens.iter().enumerate() {
            let base = command_basename(token);
            if (base == "init" || base == "telinit")
                && tokens.get(i + 1).is_some_and(|n| *n == "0" || *n == "6")
            {
                return true;
            }
            if POWER_CMDS.contains(&base) {
                if base == "systemctl" {
                    if tokens
                        .get(i + 1)
                        .is_some_and(|n| matches!(*n, "poweroff" | "reboot" | "halt" | "kexec"))
                    {
                        return true;
                    }
                } else {
                    return true;
                }
            }
        }
    }
    false
}

fn is_windows_format(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("format ")
        && (lower.contains(" c:") || lower.contains(" c ") || lower.contains(" /"))
}

fn is_pipe_to_shell_at_root(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    (lower.contains("| sh")
        || lower.contains("| bash")
        || lower.contains("|sh")
        || lower.contains("|bash"))
        && (lower.contains("curl ") || lower.contains("wget "))
}

fn split_command_segments(s: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    for ch in s.chars() {
        if matches!(ch, ';' | '&' | '|' | '\n') {
            if !current.trim().is_empty() {
                segments.push(current.trim().to_string());
            }
            current.clear();
            if ch == '&' || ch == '|' {
                // skip chained operator char
            }
        } else {
            current.push(ch);
        }
    }
    if !current.trim().is_empty() {
        segments.push(current.trim().to_string());
    }
    if segments.is_empty() {
        segments.push(s.to_string());
    }
    segments
}

fn command_basename(token: &str) -> &str {
    token.rsplit(['/', '\\']).next().unwrap_or(token)
}

/// Commands that read a file from an argument → file arguments must be
/// restricted to the cwd allowlist (otherwise `cat /etc/passwd` would leak an arbitrary
/// file in smart mode without approval).
fn reads_file_args(base: &str) -> bool {
    matches!(
        base,
        "cat" | "type" | "head" | "tail" | "ls" | "dir" | "gci" | "get-childitem"
    )
}

/// Safe read-only commands in smart mode (the whole chain).
/// `roots` = canonicalized cwd-allowlist roots; file arguments must remain
/// under them for commands that read files.
fn is_safe_readonly_command(command: &str, roots: &[PathBuf]) -> bool {
    if command.contains('>') || command.contains('<') || command.contains('|') {
        return false;
    }
    // Normalized (lowercase, backslashes removed) is used for the command-name/blocklist
    // logic; the ORIGINAL is used for path validation (normalize breaks Windows paths:
    // C:\Users\... -> c:users...). Both are segmented and paired by index.
    let norm_segments = split_command_segments(&normalize_for_detection(command));
    let raw_segments = split_command_segments(command);
    for (i, segment) in norm_segments.iter().enumerate() {
        let raw = raw_segments.get(i).map_or(segment.as_str(), String::as_str);
        if !segment_is_safe_readonly(segment, raw, roots) {
            return false;
        }
    }
    true
}

fn segment_is_safe_readonly(segment: &str, raw_segment: &str, roots: &[PathBuf]) -> bool {
    let tokens: Vec<&str> = segment.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }
    // Skip wrappers (sudo/env are not allowed in smart-auto).
    if tokens.iter().any(|t| {
        matches!(
            command_basename(t),
            "sudo" | "env" | "nohup" | "exec" | "bash" | "sh" | "cmd" | "powershell"
        )
    }) {
        return false;
    }
    let base = command_basename(tokens[0]);
    let base_ok = matches!(
        base,
        "ls" | "dir"
            | "echo"
            | "pwd"
            | "cd"
            | "cat"
            | "type"
            | "head"
            | "tail"
            | "whoami"
            | "hostname"
            | "date"
            | "uname"
            | "id"
            | "printenv"
            | "get-location"
            | "gci"
            | "get-childitem"
    );
    if !base_ok {
        return false;
    }
    // SECURITY FIX 2026-07-09: commands that read files may receive file
    // arguments ONLY from inside the allowlist. Non-file tokens (flags like -la,
    // numeric ones like `tail -n 5`) are allowed; file paths are validated against roots.
    if reads_file_args(base) {
        // Use the ORIGINAL arguments (raw_segment) — the normalized segment is
        // lowercased + backslashes removed, which breaks Windows paths.
        let raw_tokens: Vec<&str> = raw_segment.split_whitespace().collect();
        for arg in raw_tokens.iter().skip(1) {
            // Skip flags and plain numbers (e.g. `head -n 20`, `tail -5`).
            if arg.starts_with('-') || arg.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            // File argument: must canonicalize to under the allowlist.
            match std::fs::canonicalize(arg) {
                Ok(p) if path_is_under_any(&p, roots) => {}
                // Does not canonicalize (does not yet exist) OR is not in the allowlist → block.
                _ => return false,
            }
        }
    }
    true
}

fn summarize_bytes(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut summary: String = text
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect();
    if summary.len() > OUTPUT_MAX_BYTES {
        summary.truncate(OUTPUT_MAX_BYTES);
        while !summary.is_char_boundary(summary.len()) {
            summary.pop();
        }
        summary.push('…');
    }
    summary.trim().to_string()
}

#[async_trait]
impl ActionExecutor for ShellExec {
    // 2026-08-01: idempotenssi-haara tekee execute-funktiosta pitkän;
    // refaktorointi erillisiin helpereihin on mahdollista mutta riskialtista,
    // joten allow on perusteltu.
    #[allow(clippy::too_many_lines)]
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let input: ShellExecInput = match serde_json::from_value(request.payload.clone()) {
            Ok(input) => input,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid shell_exec input: {e}"),
                    request.now,
                ));
            }
        };

        if let Err(e) = self.gate_command(&input.command) {
            return Ok(ActionResult::failure(
                format!("command rejected: {e}"),
                request.now,
            ));
        }

        let cwd = match self.resolve_cwd(input.cwd.as_deref()) {
            Ok(path) => path,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("cwd rejected: {e}"),
                    request.now,
                ));
            }
        };

        // Added 2026-07-31 (per design doc): idempotency support.
        let idem_store = crate::idempotency::IdempotencyStore::from_env();
        if let Some(raw_key) = &input.idempotency_key {
            let key = match crate::idempotency::IdempotencyStore::normalize_key(raw_key) {
                Ok(k) => k,
                Err(e) => {
                    return Ok(ActionResult::failure(
                        format!("idempotency_key invalid: {e}"),
                        request.now,
                    ));
                }
            };
            if let Some(rec) = idem_store
                .lookup("shell_exec", &key)
                .map_err(|e| ActionError::Proof(format!("idempotency lookup failed: {e}")))?
            {
                if rec.state == crate::idempotency::IdemState::Completed {
                    return Ok(ActionResult::success(
                        "cached idempotent shell_exec response",
                        rec.response.clone(),
                        request.now,
                    ));
                }
                let _ = idem_store.delete("shell_exec", &key);
            }
            idem_store
                .begin("shell_exec", &key, request.payload.clone(), 0)
                .map_err(|e| ActionError::Proof(format!("idempotency begin failed: {e}")))?;
            let result = self.run_command(&input.command, &cwd).await;
            return match result {
                Ok(out) => {
                    let output = json!({
                        "exit_code": out.exit_code,
                        "stdout_summary": out.stdout_summary,
                        "stderr_summary": out.stderr_summary,
                        "timed_out": out.timed_out,
                        "idempotency_key": key,
                        "idempotency_status": "completed",
                    });
                    if out.exit_code == 0 {
                        let _ = idem_store.complete(
                            "shell_exec",
                            &key,
                            request.payload.clone(),
                            output.clone(),
                            crate::idempotency::DEFAULT_TTL_SECS,
                        );
                        Ok(ActionResult::success(
                            format!("shell exited with code {}", out.exit_code),
                            output,
                            request.now,
                        ))
                    } else {
                        // Cache failed (transient exit 1-127) per design §6.4.
                        let cacheable = (1..=127).contains(&out.exit_code);
                        let _ = idem_store.fail(
                            "shell_exec",
                            &key,
                            request.payload.clone(),
                            format!("shell exited with code {}", out.exit_code),
                            cacheable,
                            crate::idempotency::DEFAULT_TTL_SECS,
                        );
                        Ok(ActionResult {
                            status: ActionStatus::Failed,
                            output_summary: format!("shell exited with code {}", out.exit_code),
                            untrusted: true,
                            raw_output_redacted: output,
                            finished_at: request.now,
                        })
                    }
                }
                Err(e) => {
                    let _ = idem_store.fail(
                        "shell_exec",
                        &key,
                        request.payload.clone(),
                        e.to_string(),
                        false,
                        crate::idempotency::DEFAULT_TTL_SECS,
                    );
                    Ok(ActionResult::failure(e.to_string(), request.now))
                }
            };
        }

        let out = match self.run_command(&input.command, &cwd).await {
            Ok(out) => out,
            Err(e) => return Ok(ActionResult::failure(e.to_string(), request.now)),
        };

        let output = json!({
            "exit_code": out.exit_code,
            "stdout_summary": out.stdout_summary,
            "stderr_summary": out.stderr_summary,
            "timed_out": out.timed_out,
        });

        if out.timed_out {
            return Ok(ActionResult {
                status: ActionStatus::Failed,
                output_summary: "shell command timed out".to_string(),
                untrusted: true,
                raw_output_redacted: output,
                finished_at: request.now,
            });
        }

        if out.exit_code == 0 {
            Ok(ActionResult::success(
                format!("shell exited with code {}", out.exit_code),
                output,
                request.now,
            ))
        } else {
            Ok(ActionResult {
                status: ActionStatus::Failed,
                output_summary: format!("shell exited with code {}", out.exit_code),
                untrusted: true,
                raw_output_redacted: output,
                finished_at: request.now,
            })
        }
    }
}

impl Skill for ShellExec {
    fn manifest(&self) -> SkillManifest {
        let (risk, approval_policy) = match self.config.mode {
            ShellMode::Smart => (ActionRisk::ReadOnly, ApprovalPolicy::AutoIfReadOnly),
            ShellMode::Manual | ShellMode::Off => (
                ActionRisk::ExecuteCode,
                ApprovalPolicy::AlwaysRequireApproval,
            ),
        };

        SkillManifest {
            id: Self::skill_id(),
            name: "shell_exec".to_string(),
            version: "1.0.0".to_string(),
            description:
                "Executes a single command-line command with an allowlisted working directory; \
                 Hermes-style hard block; modes manual/smart/off (FAMILYCLAW_SHELL_MODE)."
                    .to_string(),
            permissions: vec![SkillPermission::ExecuteCode],
            risk,
            approval_policy,
            input_hint: Some("{ command, cwd? }".to_string()),
            output_hint: Some(
                "{ exit_code, stdout_summary, stderr_summary, timed_out }".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command-line command to execute (single line)."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Optional working directory (will be canonicalized; under the allowlist)."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            publisher: None,
            signature: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ActionId, ActionTaskId};
    use familyclaw_core::time::{from_unix_secs, Timestamp};

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "familyclaw_shell_exec_{tag}_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::canonicalize(&dir).expect("canonicalize temp dir")
    }

    fn make_request(skill_id: SkillId, payload: serde_json::Value) -> ActionRequest {
        ActionRequest::new(
            ActionId::new(),
            skill_id,
            ActionTaskId::new(),
            payload,
            at(1),
        )
    }

    #[test]
    fn manifest_manual_is_execute_code_always_require_approval() {
        let m = ShellExec::new().manifest();
        m.validate().expect("manifest validates");
        assert_eq!(m.name, "shell_exec");
        assert_eq!(m.risk, ActionRisk::ExecuteCode);
        assert_eq!(m.approval_policy, ApprovalPolicy::AlwaysRequireApproval);
        assert_eq!(m.permissions, vec![SkillPermission::ExecuteCode]);
    }

    #[test]
    fn manifest_smart_is_read_only_auto() {
        let m = ShellExec::with_config(ShellExecConfig::new().mode(ShellMode::Smart)).manifest();
        assert_eq!(m.risk, ActionRisk::ReadOnly);
        assert_eq!(m.approval_policy, ApprovalPolicy::AutoIfReadOnly);
    }

    #[test]
    fn hardline_blocks_rm_rf_root() {
        assert!(ShellExec::detect_hardline_block("rm -rf /").is_some());
        assert!(ShellExec::detect_hardline_block("rm -rf --no-preserve-root /").is_some());
        assert!(ShellExec::detect_hardline_block("rm -rf /etc").is_some());
    }

    #[test]
    fn hardline_blocks_obfuscated_rm() {
        assert!(
            ShellExec::detect_hardline_block("r\\m -rf /").is_some(),
            "backslash escape must not bypass"
        );
    }

    #[test]
    fn hardline_blocks_named_user_home() {
        // The reported destructive-agent incident wiped a HOME directory. A named
        // user-home root (not the literal `/home`) must hard-block, not merely
        // fall through to the approval gate.
        for cmd in [
            "rm -rf /home/operator",
            "rm -rf /home/operator/*",
            "rm -rf /Users/operator",
            "rm -rf /root/subuser",
        ] {
            assert!(
                ShellExec::detect_hardline_block(cmd).is_some(),
                "named user home must hard-block: {cmd}"
            );
        }
        // Deeper, specific project paths stay allowed (approval gate handles them);
        // hard-block would be too blunt and break ordinary cleanups.
        assert!(
            ShellExec::detect_hardline_block("rm -rf /home/operator/project/build").is_none(),
            "deep project subpath must not hard-block"
        );
    }

    #[test]
    fn hardline_blocks_fork_bomb() {
        assert!(ShellExec::detect_hardline_block(":(){ :|:& };:").is_some());
    }

    #[test]
    fn hardline_blocks_mkfs_and_dd() {
        assert!(ShellExec::detect_hardline_block("mkfs.ext4 /dev/sda1").is_some());
        assert!(ShellExec::detect_hardline_block("dd if=/dev/zero of=/dev/sda").is_some());
    }

    #[test]
    fn hardline_blocks_shutdown_and_kill_all() {
        assert!(ShellExec::detect_hardline_block("shutdown -h now").is_some());
        assert!(ShellExec::detect_hardline_block("kill -9 -1").is_some());
    }

    #[test]
    fn hardline_allows_benign_echo() {
        assert!(ShellExec::detect_hardline_block("echo hello").is_none());
        assert!(ShellExec::detect_hardline_block("ls -la").is_none());
    }

    #[test]
    fn safe_readonly_allows_ls_echo_pwd() {
        // Non-file commands and plain flags are ok without an allowlist.
        let no_roots: &[PathBuf] = &[];
        assert!(is_safe_readonly_command("echo hello", no_roots));
        assert!(is_safe_readonly_command("pwd", no_roots));
        assert!(is_safe_readonly_command("whoami", no_roots));
    }

    #[test]
    fn safe_readonly_rejects_redirection_and_rm() {
        let no_roots: &[PathBuf] = &[];
        assert!(!is_safe_readonly_command("echo hi > out.txt", no_roots));
        assert!(!is_safe_readonly_command("rm file.txt", no_roots));
    }

    #[test]
    fn safe_readonly_blocks_file_read_outside_allowlist() {
        // SECURITY FIX 2026-07-09 (audit [3]): cat/head/tail/ls file arguments
        // MUST remain inside the cwd allowlist. Without this, `cat /etc/passwd` ran
        // in smart mode without approval.
        let dir = temp_dir("shellsafe");
        let roots = canonicalize_roots(std::slice::from_ref(&dir));

        // File inside the allowlist → allowed.
        let inside = dir.join("ok.txt");
        std::fs::write(&inside, b"hello").unwrap();
        let inside_cmd = format!("cat {}", inside.to_string_lossy());
        assert!(
            is_safe_readonly_command(&inside_cmd, &roots),
            "allowlistin sisäinen tiedosto pitäisi sallia"
        );

        // File OUTSIDE the allowlist → blocked (previously: passed through).
        #[cfg(unix)]
        assert!(
            !is_safe_readonly_command("cat /etc/passwd", &roots),
            "cat /etc/passwd EI saa läpäistä smart-tilaa"
        );
        assert!(
            !is_safe_readonly_command("head ../../secret.txt", &roots),
            "allowlistin ulkopuolinen head EI saa läpäistä"
        );
        // Flags and numbers without a file are ok (head -n 5 without a path).
        assert!(is_safe_readonly_command("date", &roots));
    }

    #[tokio::test]
    async fn off_mode_rejects_without_execution() {
        let dir = temp_dir("off");
        let skill =
            ShellExec::with_config(ShellExecConfig::new().mode(ShellMode::Off).allow_cwd(&dir));
        let payload = json!({ "command": "echo hi", "cwd": dir.to_string_lossy() });
        let res = skill
            .execute(make_request(ShellExec::skill_id(), payload))
            .await
            .expect("execute");
        assert!(!res.status.is_success());
        assert!(res.output_summary.contains("rejected"));
    }

    #[tokio::test]
    async fn empty_cwd_allowlist_rejects() {
        let skill = ShellExec::new();
        let payload = json!({ "command": "echo hi" });
        let res = skill
            .execute(make_request(ShellExec::skill_id(), payload))
            .await
            .expect("execute");
        assert!(!res.status.is_success());
        assert!(res.output_summary.contains("rejected"));
    }

    #[tokio::test]
    async fn smart_mode_rejects_non_readonly_command() {
        let dir = temp_dir("smart_reject");
        let skill = ShellExec::with_config(
            ShellExecConfig::new()
                .mode(ShellMode::Smart)
                .allow_cwd(&dir),
        );
        let payload = json!({
            "command": "touch newfile.txt",
            "cwd": dir.to_string_lossy()
        });
        let res = skill
            .execute(make_request(ShellExec::skill_id(), payload))
            .await
            .expect("execute");
        assert!(!res.status.is_success());
        assert!(res.output_summary.contains("rejected"));
    }

    #[tokio::test]
    async fn executes_echo_in_allowlisted_cwd() {
        let dir = temp_dir("echo_ok");
        let skill = ShellExec::with_config(ShellExecConfig::new().allow_cwd(&dir));

        #[cfg(windows)]
        let command = "echo shell_ok";
        #[cfg(not(windows))]
        let command = "echo shell_ok";

        let payload = json!({
            "command": command,
            "cwd": dir.to_string_lossy()
        });
        let res = skill
            .execute(make_request(ShellExec::skill_id(), payload))
            .await
            .expect("execute");
        assert!(
            res.status.is_success(),
            "echo must succeed: {}",
            res.output_summary
        );
        assert_eq!(res.raw_output_redacted["exit_code"], json!(0));
        let stdout = res.raw_output_redacted["stdout_summary"]
            .as_str()
            .unwrap_or("");
        assert!(stdout.contains("shell_ok"), "stdout was: {stdout}");
    }

    #[tokio::test]
    async fn hardline_blocks_even_in_manual_mode() {
        let dir = temp_dir("hardline");
        let skill = ShellExec::with_config(ShellExecConfig::new().allow_cwd(&dir));
        let payload = json!({
            "command": "rm -rf /",
            "cwd": dir.to_string_lossy()
        });
        let res = skill
            .execute(make_request(ShellExec::skill_id(), payload))
            .await
            .expect("execute");
        assert!(!res.status.is_success());
        assert!(res.output_summary.contains("hardline"));
    }

    #[test]
    fn shell_mode_parse() {
        assert_eq!(ShellMode::parse("smart"), ShellMode::Smart);
        assert_eq!(ShellMode::parse("OFF"), ShellMode::Off);
        assert_eq!(ShellMode::parse("manual"), ShellMode::Manual);
        assert_eq!(ShellMode::parse("unknown"), ShellMode::Manual);
    }

    #[test]
    fn from_env_reads_mode() {
        // Smoke test only: from_env does not crash without environment variables.
        let cfg = ShellExecConfig::from_env();
        assert_eq!(cfg.shell_mode(), ShellMode::Manual);
    }
}
