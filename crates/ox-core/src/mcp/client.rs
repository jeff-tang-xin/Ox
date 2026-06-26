//! Minimal stdio JSON-RPC 2.0 client for a single MCP server child process.
//!
//! Transport: newline-delimited JSON-RPC over the child's stdin/stdout (the MCP
//! stdio convention — one JSON message per line, no Content-Length framing).
//!
//! A background task drains the server's stdout and routes each response back to
//! the awaiting caller by request id via a oneshot channel. Server-initiated
//! notifications (no `id`) are logged and dropped — Ox only drives request/reply.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};

/// MCP protocol revision Ox negotiates during `initialize`.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// How to launch a stdio MCP server.
#[derive(Debug, Clone)]
pub struct McpServerSpec {
    /// Program to execute (e.g. `npx`, `gitnexus`, or an absolute path).
    pub command: String,
    /// Full argument vector passed to the program.
    pub args: Vec<String>,
    /// Working directory for the child (usually the project root). `None` inherits.
    pub cwd: Option<PathBuf>,
}

impl McpServerSpec {
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            cwd: None,
        }
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }
}

type PendingMap = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;

/// Build a child command, transparently handling Windows batch launchers.
///
/// On Windows, `npx`/`npm`/`*.cmd`/`*.bat` are shell shims, not PE executables —
/// `CreateProcess` can't run them directly (this is why `Command::new("npx")`
/// fails with "program not found"). Routing them through `cmd.exe /C` lets the
/// shell resolve the launcher name + extension from `PATH` and execute the shim.
/// Real executables (absolute/relative `.exe` paths) are spawned directly. On
/// non-Windows platforms the program is always spawned directly.
pub(crate) fn build_command(program: &str, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        let lower = program.to_ascii_lowercase();
        let is_path = program.contains('/') || program.contains('\\');
        let is_batch = lower.ends_with(".cmd") || lower.ends_with(".bat");
        // Bare names (e.g. `npx`, `gitnexus`) and batch shims must go via cmd.exe.
        if !is_path || is_batch {
            let mut cmd = Command::new("cmd");
            cmd.arg("/C").arg(program).args(args);
            return cmd;
        }
    }
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd
}

/// A live connection to one stdio MCP server.
///
/// Dropping the client kills the child process (`kill_on_drop`). Call
/// [`McpClient::shutdown`] for a deterministic, awaited teardown.
pub struct McpClient {
    child: Mutex<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: PendingMap,
    next_id: AtomicI64,
    request_timeout: Duration,
    /// Human-readable label for logs/errors (the launch command).
    label: String,
}

impl McpClient {
    /// Spawn the server and start the stdout reader. Does **not** perform the
    /// MCP handshake — call [`McpClient::initialize`] next.
    pub async fn connect(spec: &McpServerSpec, request_timeout: Duration) -> anyhow::Result<Self> {
        let mut cmd = build_command(&spec.command, &spec.args);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        // On Unix the launcher (`npx`) forks the real server (`node`) as a child,
        // so put the whole thing in its own process group; shutdown then signals
        // the group (negative PID) to reap the tree, mirroring the Windows
        // `taskkill /T`. Without this, an orphaned reader keeps the index DB open
        // and breaks the next reindex/restart.
        #[cfg(unix)]
        cmd.process_group(0);
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "failed to spawn MCP server `{} {}`",
                spec.command,
                spec.args.join(" ")
            )
        })?;

        let stdin = child
            .stdin
            .take()
            .context("MCP server stdin not captured")?;
        let stdout = child
            .stdout
            .take()
            .context("MCP server stdout not captured")?;

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        // Background reader: route id'd responses to awaiting callers.
        {
            let pending = pending.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<Value>(line) {
                                Ok(msg) => {
                                    if let Some(id) = msg.get("id").and_then(Value::as_i64) {
                                        if let Some(tx) = pending.lock().await.remove(&id) {
                                            let _ = tx.send(msg);
                                        }
                                    } else {
                                        tracing::trace!("[MCP] notification: {line}");
                                    }
                                }
                                Err(e) => {
                                    tracing::debug!("[MCP] skipping non-JSON line ({e}): {line}");
                                }
                            }
                        }
                        Ok(None) => {
                            tracing::info!("[MCP] server stdout closed; reader stopping");
                            break;
                        }
                        Err(e) => {
                            tracing::warn!("[MCP] stdout read error: {e}");
                            break;
                        }
                    }
                }
                // Fail any in-flight requests so callers don't hang forever.
                pending.lock().await.clear();
            });
        }

        Ok(Self {
            child: Mutex::new(child),
            stdin: Arc::new(Mutex::new(stdin)),
            pending,
            next_id: AtomicI64::new(1),
            request_timeout,
            label: format!("{} {}", spec.command, spec.args.join(" ")),
        })
    }

    /// Perform the MCP `initialize` handshake followed by the
    /// `notifications/initialized` acknowledgement. Returns the server's
    /// `initialize` result (capabilities, serverInfo, ...).
    pub async fn initialize(
        &self,
        client_name: &str,
        client_version: &str,
    ) -> anyhow::Result<Value> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": client_name, "version": client_version },
        });
        let result = self.send_request("initialize", params).await?;
        self.send_notification("notifications/initialized", json!({}))
            .await?;
        Ok(result)
    }

    /// `tools/list` — discover the tools the server exposes.
    pub async fn list_tools(&self) -> anyhow::Result<Value> {
        self.send_request("tools/list", json!({})).await
    }

    /// `tools/call` — invoke a tool by name with JSON arguments.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> anyhow::Result<Value> {
        self.send_request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    /// Whether the child process is still running.
    pub async fn is_alive(&self) -> bool {
        matches!(self.child.lock().await.try_wait(), Ok(None))
    }

    /// Best-effort, awaited teardown of the child process.
    pub async fn shutdown(&self) {
        // On Windows the launcher runs under `cmd.exe`, so the real server is a
        // grandchild (`cmd → node`). Killing only `cmd` would orphan `node`,
        // which keeps the index DB open and breaks the next reindex/restart.
        // `taskkill /T` tears down the whole tree by PID.
        #[cfg(windows)]
        {
            let pid = self.child.lock().await.id();
            if let Some(pid) = pid {
                let _ = Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            }
        }
        // Unix: the child leads its own process group (set in `connect`), so a
        // negative PID signals the whole tree (`npx` → `node`).
        #[cfg(unix)]
        {
            let pid = self.child.lock().await.id();
            if let Some(pid) = pid {
                let _ = Command::new("kill")
                    .arg("-KILL")
                    .arg(format!("-{pid}"))
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            }
        }
        let mut child = self.child.lock().await;
        let _ = child.start_kill();
        let _ = child.wait().await;
    }

    // ──────────────────────────── internals ────────────────────────────

    async fn send_request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = self.write_line(&msg).await {
            self.pending.lock().await.remove(&id);
            return Err(e).with_context(|| {
                format!(
                    "MCP write failed for `{method}` (server `{}` down?)",
                    self.label
                )
            });
        }

        let resp = match tokio::time::timeout(self.request_timeout, rx).await {
            Err(_) => {
                self.pending.lock().await.remove(&id);
                anyhow::bail!(
                    "MCP `{method}` timed out after {:?} (server `{}`)",
                    self.request_timeout,
                    self.label
                );
            }
            Ok(Err(_)) => {
                anyhow::bail!(
                    "MCP `{method}` channel closed — server `{}` died",
                    self.label
                );
            }
            Ok(Ok(resp)) => resp,
        };

        if let Some(err) = resp.get("error") {
            anyhow::bail!("MCP `{method}` returned error: {err}");
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn send_notification(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.write_line(&msg).await
    }

    async fn write_line(&self, msg: &Value) -> anyhow::Result<()> {
        let mut line = serde_json::to_string(msg)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }
}
