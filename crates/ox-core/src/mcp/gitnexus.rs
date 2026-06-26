//! GitNexus code-graph service.
//!
//! Wraps the [`McpClient`](super::client::McpClient) with lifecycle management
//! (spawn → `initialize` → health/restart) and a **typed API over every
//! GitNexus capability**. Graph queries go over MCP; index lifecycle
//! (`analyze` / `status`) is delegated to the same binary's CLI subcommands
//! run as one-shot child processes.
//!
//! Capability map (MCP tools):
//! - Comprehension: [`query`](GitNexusService::query),
//!   [`context`](GitNexusService::context), [`cypher`](GitNexusService::cypher),
//!   [`list_repos`](GitNexusService::list_repos)
//! - Pre-change impact: [`impact`](GitNexusService::impact),
//!   [`detect_changes`](GitNexusService::detect_changes),
//!   [`api_impact`](GitNexusService::api_impact)
//! - API surface maps: [`route_map`](GitNexusService::route_map),
//!   [`tool_map`](GitNexusService::tool_map),
//!   [`shape_check`](GitNexusService::shape_check)
//! - Refactor: [`rename`](GitNexusService::rename)
//! - Multi-repo groups: [`group_list`](GitNexusService::group_list),
//!   [`group_sync`](GitNexusService::group_sync)
//!
//! CLI ops: [`cli_analyze`](GitNexusService::cli_analyze),
//! [`cli_status`](GitNexusService::cli_status).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::Context;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::config::GitNexusConfig;

use super::client::{McpClient, McpServerSpec};

/// Live state of the GitNexus child process / connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitNexusStatus {
    /// Integration switched off in config.
    Disabled,
    /// Configured but not yet started.
    NotStarted,
    /// Spawn + handshake in progress.
    Starting,
    /// Connected and ready to serve queries.
    Ready,
    /// Last start attempt failed; carries the reason.
    Failed(String),
}

impl GitNexusStatus {
    pub fn label(&self) -> String {
        match self {
            GitNexusStatus::Disabled => "disabled".into(),
            GitNexusStatus::NotStarted => "not started".into(),
            GitNexusStatus::Starting => "starting".into(),
            GitNexusStatus::Ready => "ready".into(),
            GitNexusStatus::Failed(e) => format!("failed: {e}"),
        }
    }
}

/// Normalized result of an MCP `tools/call`.
#[derive(Debug, Clone)]
pub struct GraphResult {
    /// Concatenated text content (usually JSON or Markdown the tool produced).
    pub text: String,
    /// Whether the tool reported an error (`isError: true`).
    pub is_error: bool,
    /// The full raw `tools/call` result envelope.
    pub raw: Value,
}

impl GraphResult {
    fn from_call(raw: Value) -> Self {
        let is_error = raw.get("isError").and_then(Value::as_bool).unwrap_or(false);
        let text = match raw.get("content").and_then(Value::as_array) {
            Some(items) => items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            None => String::new(),
        };
        Self {
            text,
            is_error,
            raw,
        }
    }
}

/// Result of a one-shot CLI subcommand (`analyze`, `status`).
#[derive(Debug, Clone)]
pub struct CliResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

// ──────────────────────────── typed params ────────────────────────────
//
// One struct per GitNexus tool. Optional fields are skipped when `None` so the
// server sees exactly the arguments the caller set. Field names mirror the MCP
// schema; camelCase wire names are mapped via `#[serde(rename)]`.

/// `query` — execution flows for a concept (BM25 + semantic).
#[derive(Debug, Clone, Default, Serialize)]
pub struct QueryParams {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_symbols: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_content: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
}

impl QueryParams {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            ..Default::default()
        }
    }
}

/// `context` — 360° view of a single symbol.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ContextParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_content: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
}

impl ContextParams {
    pub fn by_name(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            ..Default::default()
        }
    }
    pub fn by_uid(uid: impl Into<String>) -> Self {
        Self {
            uid: Some(uid.into()),
            ..Default::default()
        }
    }
}

/// `cypher` — raw Cypher against the knowledge graph.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CypherParams {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// `impact` — blast radius of changing a symbol.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ImpactParams {
    pub target: String,
    /// `"upstream"` (what depends on this) or `"downstream"` (what this depends on).
    pub direction: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(rename = "maxDepth", skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    #[serde(rename = "crossDepth", skip_serializing_if = "Option::is_none")]
    pub cross_depth: Option<u32>,
    #[serde(rename = "relationTypes", skip_serializing_if = "Option::is_none")]
    pub relation_types: Option<Vec<String>>,
    #[serde(rename = "includeTests", skip_serializing_if = "Option::is_none")]
    pub include_tests: Option<bool>,
    #[serde(rename = "minConfidence", skip_serializing_if = "Option::is_none")]
    pub min_confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subgroup: Option<String>,
    #[serde(rename = "timeoutMs", skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl ImpactParams {
    pub fn new(target: impl Into<String>, direction: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            direction: direction.into(),
            ..Default::default()
        }
    }
}

/// `detect_changes` — map uncommitted git diff to affected flows.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DetectChangesParams {
    /// `"unstaged"` (default), `"staged"`, `"all"`, or `"compare"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// `api_impact` — pre-change report for an API route handler.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ApiImpactParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// `route_map` — route → handler/consumer mappings.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RouteMapParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// `tool_map` — MCP/RPC tool definitions.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ToolMapParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// `shape_check` — response-shape vs consumer-access mismatch.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ShapeCheckParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// `rename` — coordinated multi-file rename (preview by default).
#[derive(Debug, Clone, Default, Serialize)]
pub struct RenameParams {
    pub new_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

impl RenameParams {
    pub fn new(symbol_name: impl Into<String>, new_name: impl Into<String>) -> Self {
        Self {
            new_name: new_name.into(),
            symbol_name: Some(symbol_name.into()),
            ..Default::default()
        }
    }
}

/// `group_sync` — rebuild a group's contract registry.
#[derive(Debug, Clone, Default, Serialize)]
pub struct GroupSyncParams {
    pub name: String,
    #[serde(rename = "skipEmbeddings", skip_serializing_if = "Option::is_none")]
    pub skip_embeddings: Option<bool>,
    #[serde(rename = "exactOnly", skip_serializing_if = "Option::is_none")]
    pub exact_only: Option<bool>,
}

// ──────────────────────────── service ────────────────────────────

/// Owns the GitNexus child process and exposes its full capability surface.
pub struct GitNexusService {
    config: GitNexusConfig,
    spec: McpServerSpec,
    /// Working directory for both MCP server and CLI ops (project root).
    project_root: PathBuf,
    client: Mutex<Option<Arc<McpClient>>>,
    status: Mutex<GitNexusStatus>,
    /// Serializes start/restart so concurrent ops don't spawn duplicates.
    start_lock: Mutex<()>,
    /// Set when Ox edits/writes/deletes a file (E): the index is behind the tree.
    dirty: AtomicBool,
    /// Serializes on-change reindex so concurrent queries don't double-analyze.
    refresh_lock: Mutex<()>,
}

impl GitNexusService {
    /// Construct from config + project root. Does not spawn anything yet.
    pub fn new(config: GitNexusConfig, project_root: PathBuf) -> Self {
        let spec = McpServerSpec::new(config.command.clone(), config.mcp_args())
            .with_cwd(project_root.clone());
        let initial = if config.enabled {
            GitNexusStatus::NotStarted
        } else {
            GitNexusStatus::Disabled
        };
        Self {
            config,
            spec,
            project_root,
            client: Mutex::new(None),
            status: Mutex::new(initial),
            start_lock: Mutex::new(()),
            dirty: AtomicBool::new(false),
            refresh_lock: Mutex::new(()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub async fn status(&self) -> GitNexusStatus {
        self.status.lock().await.clone()
    }

    /// Mark the integration as unavailable with a user-facing reason. Used by the
    /// mandatory-mode startup path when the toolchain (Node/npx or the configured
    /// launcher) can't be resolved, so the per-turn gate blocks with guidance.
    pub async fn mark_unavailable(&self, reason: impl Into<String>) {
        self.set_status(GitNexusStatus::Failed(reason.into())).await;
    }

    /// Ready to serve a query *right now* without spawning/restarting: status is
    /// `Ready` and the child process is alive. Used by latency-sensitive callers
    /// (e.g. `find_symbol` enrichment) that must never trigger a cold start.
    pub async fn is_ready(&self) -> bool {
        if !matches!(*self.status.lock().await, GitNexusStatus::Ready) {
            return false;
        }
        match self.client.lock().await.as_ref() {
            Some(c) => c.is_alive().await,
            None => false,
        }
    }

    async fn set_status(&self, s: GitNexusStatus) {
        *self.status.lock().await = s;
    }

    /// Spawn the MCP server and complete the `initialize` handshake. Idempotent
    /// when already `Ready` with a live client. Returns the live client.
    pub async fn start(&self) -> anyhow::Result<Arc<McpClient>> {
        if !self.config.enabled {
            anyhow::bail!("GitNexus integration is disabled in config");
        }
        let _guard = self.start_lock.lock().await;

        // Fast path: someone already started a live client.
        if let Some(c) = self.client.lock().await.as_ref() {
            if c.is_alive().await {
                return Ok(c.clone());
            }
        }

        self.set_status(GitNexusStatus::Starting).await;
        // Handshake may include a first-run package download, so honor the
        // larger of the two budgets for the client's request timeout.
        let timeout = Duration::from_millis(
            self.config
                .request_timeout_ms
                .max(self.config.startup_timeout_ms),
        );

        let client = match McpClient::connect(&self.spec, timeout).await {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("spawn failed: {e}");
                self.set_status(GitNexusStatus::Failed(msg.clone())).await;
                return Err(e);
            }
        };

        let init = tokio::time::timeout(
            Duration::from_millis(self.config.startup_timeout_ms),
            client.initialize("ox", env!("CARGO_PKG_VERSION")),
        )
        .await;

        match init {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                client.shutdown().await;
                self.set_status(GitNexusStatus::Failed(format!("handshake error: {e}")))
                    .await;
                return Err(e);
            }
            Err(_) => {
                client.shutdown().await;
                let msg = format!(
                    "handshake timed out after {}ms",
                    self.config.startup_timeout_ms
                );
                self.set_status(GitNexusStatus::Failed(msg.clone())).await;
                anyhow::bail!(msg);
            }
        }

        let client = Arc::new(client);
        *self.client.lock().await = Some(client.clone());
        self.set_status(GitNexusStatus::Ready).await;
        tracing::info!(
            "[GitNexus] ready ({} {})",
            self.spec.command,
            self.spec.args.join(" ")
        );
        Ok(client)
    }

    /// Return a live client, transparently restarting a dead server.
    async fn live_client(&self) -> anyhow::Result<Arc<McpClient>> {
        if let Some(c) = self.client.lock().await.as_ref() {
            if c.is_alive().await {
                return Ok(c.clone());
            }
        }
        tracing::warn!("[GitNexus] server not alive — restarting");
        self.start().await
    }

    /// Low-level escape hatch: call any GitNexus tool by name with raw args.
    pub async fn call(&self, tool: &str, args: Value) -> anyhow::Result<GraphResult> {
        let client = self.live_client().await?;
        let raw = client
            .call_tool(tool, args)
            .await
            .with_context(|| format!("GitNexus tool `{tool}` failed"))?;
        Ok(GraphResult::from_call(raw))
    }

    async fn call_typed<P: Serialize>(
        &self,
        tool: &str,
        params: &P,
    ) -> anyhow::Result<GraphResult> {
        let args = serde_json::to_value(params)
            .with_context(|| format!("serializing args for GitNexus `{tool}`"))?;
        self.call(tool, args).await
    }

    /// List discovered MCP tools (handshake-level capability probe).
    pub async fn list_mcp_tools(&self) -> anyhow::Result<Value> {
        self.live_client().await?.list_tools().await
    }

    // ── Comprehension ──────────────────────────────────────────────
    pub async fn query(&self, params: &QueryParams) -> anyhow::Result<GraphResult> {
        self.call_typed("query", params).await
    }
    pub async fn context(&self, params: &ContextParams) -> anyhow::Result<GraphResult> {
        self.call_typed("context", params).await
    }
    pub async fn cypher(&self, params: &CypherParams) -> anyhow::Result<GraphResult> {
        self.call_typed("cypher", params).await
    }
    pub async fn list_repos(&self) -> anyhow::Result<GraphResult> {
        self.call("list_repos", Value::Object(Default::default()))
            .await
    }

    // ── Pre-change impact ──────────────────────────────────────────
    pub async fn impact(&self, params: &ImpactParams) -> anyhow::Result<GraphResult> {
        self.call_typed("impact", params).await
    }
    pub async fn detect_changes(
        &self,
        params: &DetectChangesParams,
    ) -> anyhow::Result<GraphResult> {
        self.call_typed("detect_changes", params).await
    }
    pub async fn api_impact(&self, params: &ApiImpactParams) -> anyhow::Result<GraphResult> {
        self.call_typed("api_impact", params).await
    }

    // ── API surface maps ───────────────────────────────────────────
    pub async fn route_map(&self, params: &RouteMapParams) -> anyhow::Result<GraphResult> {
        self.call_typed("route_map", params).await
    }
    pub async fn tool_map(&self, params: &ToolMapParams) -> anyhow::Result<GraphResult> {
        self.call_typed("tool_map", params).await
    }
    pub async fn shape_check(&self, params: &ShapeCheckParams) -> anyhow::Result<GraphResult> {
        self.call_typed("shape_check", params).await
    }

    // ── Refactor ───────────────────────────────────────────────────
    pub async fn rename(&self, params: &RenameParams) -> anyhow::Result<GraphResult> {
        self.call_typed("rename", params).await
    }

    // ── Multi-repo groups ──────────────────────────────────────────
    pub async fn group_list(&self, name: Option<&str>) -> anyhow::Result<GraphResult> {
        let args = match name {
            Some(n) => serde_json::json!({ "name": n }),
            None => Value::Object(Default::default()),
        };
        self.call("group_list", args).await
    }
    pub async fn group_sync(&self, params: &GroupSyncParams) -> anyhow::Result<GraphResult> {
        self.call_typed("group_sync", params).await
    }

    // ── CLI ops (index lifecycle, run as one-shot processes) ───────

    /// Run `gitnexus analyze` (build/refresh the index for the project root).
    pub async fn cli_analyze(&self) -> anyhow::Result<CliResult> {
        self.run_cli("analyze").await
    }

    /// Run `gitnexus status` (index freshness / staleness report).
    pub async fn cli_status(&self) -> anyhow::Result<CliResult> {
        self.run_cli("status").await
    }

    async fn run_cli(&self, sub: &str) -> anyhow::Result<CliResult> {
        let args = self.config.cli_args(sub);
        let output = super::client::build_command(&self.config.command, &args)
            .current_dir(&self.project_root)
            .output()
            .await
            .with_context(|| {
                format!("failed to run `{} {}`", self.config.command, args.join(" "))
            })?;
        Ok(CliResult {
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    // ── Freshness (B) + on-change reindex (E) ─────────────────────

    /// Mark the index as behind the working tree (call after Ox edits a file).
    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::SeqCst);
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::SeqCst)
    }

    /// (B) Decide whether a startup reindex is needed: true when the repo was
    /// never indexed, or any source file is newer than the index build time.
    pub async fn needs_startup_reindex(&self) -> bool {
        let root = self.project_root.clone();
        // fs walking is blocking; keep it off the async reactor.
        tokio::task::spawn_blocking(move || match index_built_at(&root) {
            None => true,
            Some(built_at) => source_tree_newer_than(&root, built_at),
        })
        .await
        .unwrap_or(true)
    }

    /// (E) If edits happened since the last index, stop the reader, run
    /// `analyze`, and clear the dirty flag. The next query respawns the reader
    /// on the fresh index via [`live_client`](Self::live_client). No-op when clean.
    ///
    /// Returns `true` when a reindex actually ran.
    pub async fn ensure_fresh_for_query(&self) -> bool {
        if !self.dirty.load(Ordering::SeqCst) {
            return false;
        }
        let _guard = self.refresh_lock.lock().await;
        // Re-check under the lock: a concurrent query may have just refreshed.
        if !self.dirty.swap(false, Ordering::SeqCst) {
            return false;
        }
        tracing::info!("[GitNexus] edits since last index — reindexing before query");
        // Stop the reader so the CLI writer never shares KuzuDB with it.
        self.shutdown().await;
        match self.cli_analyze().await {
            Ok(r) if r.success => tracing::info!("[GitNexus] on-change reindex complete"),
            Ok(r) => {
                tracing::warn!(
                    "[GitNexus] on-change reindex exited {:?}: {}",
                    r.exit_code,
                    r.stderr.trim()
                );
                // Build failed — stay dirty so we retry on the next query.
                self.dirty.store(true, Ordering::SeqCst);
            }
            Err(e) => {
                tracing::warn!("[GitNexus] on-change reindex failed: {e}");
                self.dirty.store(true, Ordering::SeqCst);
            }
        }
        true
    }

    /// Tear down the child process (if any).
    pub async fn shutdown(&self) {
        if let Some(c) = self.client.lock().await.take() {
            c.shutdown().await;
        }
        self.set_status(GitNexusStatus::NotStarted).await;
    }
}

// ──────────────────────────── freshness helpers ────────────────────────────

/// When was this repo's GitNexus index built?
///
/// Authoritative source: `~/.gitnexus/registry.json` (`indexedAt` for the entry
/// whose `path` matches the project root). Falls back to the newest mtime under
/// `<root>/.gitnexus/` if the registry is missing/unreadable. `None` ⇒ never indexed.
fn index_built_at(project_root: &Path) -> Option<SystemTime> {
    if let Some(t) = registry_indexed_at(project_root) {
        return Some(t);
    }
    // Fallback: the index dir itself.
    let index_dir = project_root.join(".gitnexus");
    newest_mtime_under(&index_dir)
}

fn registry_indexed_at(project_root: &Path) -> Option<SystemTime> {
    let registry = dirs::home_dir()?.join(".gitnexus").join("registry.json");
    let content = std::fs::read_to_string(&registry).ok()?;
    let entries: Value = serde_json::from_str(&content).ok()?;
    let target = canonical_key(project_root);
    for entry in entries.as_array()? {
        let path = entry.get("path").and_then(Value::as_str)?;
        if canonical_key(Path::new(path)) == target {
            let ts = entry.get("indexedAt").and_then(Value::as_str)?;
            let parsed = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
            return Some(parsed.with_timezone(&chrono::Utc).into());
        }
    }
    None
}

/// Normalize a path for comparison (canonicalize when possible; lowercase on
/// Windows where the filesystem is case-insensitive).
fn canonical_key(p: &Path) -> String {
    let canon = dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = canon.to_string_lossy().to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

fn newest_mtime_under(dir: &Path) -> Option<SystemTime> {
    if !dir.exists() {
        return None;
    }
    let mut newest: Option<SystemTime> = None;
    for entry in walkdir::WalkDir::new(dir).into_iter().flatten() {
        if entry.file_type().is_file() {
            if let Ok(md) = entry.metadata() {
                if let Ok(m) = md.modified() {
                    newest = Some(match newest {
                        Some(n) if n >= m => n,
                        _ => m,
                    });
                }
            }
        }
    }
    newest
}

/// Is any (gitignore-respecting) source file newer than `built_at`?
/// Early-exits on the first newer file; bounded to avoid pathological repos.
fn source_tree_newer_than(root: &Path, built_at: SystemTime) -> bool {
    const MAX_FILES: usize = 200_000;
    let mut seen = 0usize;
    let walker = ignore::WalkBuilder::new(root)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();
    for result in walker {
        let Ok(entry) = result else { continue };
        let path = entry.path();
        if path.components().any(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some(".git") | Some(".gitnexus") | Some("target") | Some("node_modules")
            )
        }) {
            continue;
        }
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            seen += 1;
            if seen > MAX_FILES {
                break;
            }
            if let Ok(md) = entry.metadata() {
                if let Ok(m) = md.modified() {
                    if m > built_at {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_params_skip_none() {
        let p = QueryParams::new("auth flow");
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["query"], "auth flow");
        assert!(v.get("limit").is_none(), "None fields must be omitted");
        assert!(v.get("repo").is_none());
    }

    #[test]
    fn impact_params_camelcase_wire_names() {
        let mut p = ImpactParams::new("validateUser", "upstream");
        p.max_depth = Some(2);
        p.relation_types = Some(vec!["CALLS".into(), "IMPORTS".into()]);
        p.include_tests = Some(true);
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["target"], "validateUser");
        assert_eq!(v["direction"], "upstream");
        assert_eq!(v["maxDepth"], 2);
        assert_eq!(v["relationTypes"][0], "CALLS");
        assert_eq!(v["includeTests"], true);
        // snake_case Rust field must not leak onto the wire.
        assert!(v.get("max_depth").is_none());
    }

    #[test]
    fn rename_defaults_dry_run_unset_means_server_default() {
        let p = RenameParams::new("oldName", "newName");
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["new_name"], "newName");
        assert_eq!(v["symbol_name"], "oldName");
        // dry_run omitted → server applies its safe default (true).
        assert!(v.get("dry_run").is_none());
    }

    #[test]
    fn graph_result_extracts_text_and_error_flag() {
        let raw = serde_json::json!({
            "content": [{ "type": "text", "text": "hello" }, { "type": "text", "text": "world" }],
            "isError": false
        });
        let r = GraphResult::from_call(raw);
        assert_eq!(r.text, "hello\nworld");
        assert!(!r.is_error);
    }

    #[test]
    fn dirty_flag_roundtrips() {
        let svc = GitNexusService::new(GitNexusConfig::default(), PathBuf::from("."));
        assert!(!svc.is_dirty());
        svc.mark_dirty();
        assert!(svc.is_dirty());
    }

    #[test]
    fn ensure_fresh_noop_when_clean() {
        let svc = GitNexusService::new(GitNexusConfig::default(), PathBuf::from("."));
        // Clean → must not attempt any reindex (returns false without spawning).
        let ran = futures::executor::block_on(svc.ensure_fresh_for_query());
        assert!(!ran);
    }

    #[test]
    fn unindexed_repo_needs_reindex() {
        let dir = std::env::temp_dir().join("ox_gn_fresh_test_unindexed");
        let _ = std::fs::create_dir_all(&dir);
        // No .gitnexus and (almost certainly) no registry entry → stale.
        let built = index_built_at(&dir);
        assert!(
            built.is_none(),
            "fresh temp dir should have no index record"
        );
    }

    #[test]
    fn disabled_config_yields_disabled_status() {
        let cfg = GitNexusConfig {
            enabled: false,
            ..Default::default()
        };
        let svc = GitNexusService::new(cfg, PathBuf::from("."));
        // status() is async; check via blocking executor-free path:
        let st = futures::executor::block_on(svc.status());
        assert_eq!(st, GitNexusStatus::Disabled);
    }
}
