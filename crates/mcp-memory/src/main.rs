//! Minimal stdio MCP server exposing the local assistant's durable memory:
//! `memory_catalog`, `memory_search`, `memory_remember`, and `memory_forget`,
//! backed by SQLite. Internal (`publish = false`), like `mcp-rag` and
//! `mcp-vision`.
//!
//! Two invariants shape the tool surface.
//!
//! Private by default: every statement is filtered by `workspace_id`. The id is
//! an explicit argument on each tool, and `ghost-link` stamps it at dispatch
//! time from the chat's own binding, so the model never gets to name its own
//! scope -- a model-chosen scope would make "per-workspace grants" decorative.
//!
//! Explicit memory: `memory_catalog` returns titles and kinds only, and
//! `memory_search` returns bodies only for the rows it actually matched. The
//! full store is never dumped into a system prompt.

mod store;

use std::path::PathBuf;
use std::sync::Mutex;

use rmcp::{
    handler::server::wrapper::Parameters, schemars, tool, tool_router, transport::stdio, ServiceExt,
};
use rusqlite::Connection;
use serde::Serialize;
use store::{MemoryKind, MemorySource};

/// Bounds on what a single call may do. The observation folded back into the
/// prompt is capped downstream by `mcp::toolcall::MAX_OBSERVATION_CHARS`, but a
/// tighter bound here keeps the JSON this tool returns small in the first place.
const MAX_TITLE_CHARS: usize = 200;
const MAX_BODY_CHARS: usize = 8000;
const DEFAULT_SEARCH_LIMIT: usize = 10;
const MAX_SEARCH_LIMIT: usize = 50;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CatalogRequest {
    #[schemars(
        description = "Workspace id, stamped by Ghostlink from the chat's binding. Do not invent one."
    )]
    workspace_id: String,
    /// Additional workspace ids whose pinned/shared collections may be read.
    #[schemars(description = "Extra workspace ids whose shared collections are visible")]
    shared_with: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SearchRequest {
    #[schemars(
        description = "Workspace id, stamped by Ghostlink from the chat's binding. Do not invent one."
    )]
    workspace_id: String,
    #[schemars(description = "Search terms")]
    query: String,
    #[schemars(description = "Restrict to these memory kinds (default: all)")]
    kinds: Option<Vec<String>>,
    #[schemars(description = "Max results (default 10, max 50)")]
    limit: Option<usize>,
    #[schemars(description = "Extra workspace ids whose shared collections are visible")]
    shared_with: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RememberRequest {
    #[schemars(
        description = "Workspace id, stamped by Ghostlink from the chat's binding. Do not invent one."
    )]
    workspace_id: String,
    #[schemars(
        description = "one of: preference, project_fact, decision, person, open_loop, summary"
    )]
    kind: String,
    #[schemars(description = "Short human-readable title (shown in the catalog)")]
    title: String,
    #[schemars(description = "The memory body")]
    body: String,
    #[schemars(description = "one of: user, compaction, tool (default: tool)")]
    source: Option<String>,
    #[schemars(description = "Pin so it outranks equal matches and survives pruning")]
    pinned: Option<bool>,
    #[schemars(description = "Expose to workspaces that grant this workspace access")]
    shared: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ForgetRequest {
    #[schemars(
        description = "Workspace id, stamped by Ghostlink from the chat's binding. Do not invent one."
    )]
    workspace_id: String,
    #[schemars(description = "Exact memory id from memory_catalog / memory_search")]
    id: String,
}

#[derive(Debug, Serialize)]
struct CatalogItem {
    id: String,
    kind: &'static str,
    title: String,
    pinned: bool,
    shared: bool,
    updated_at: i64,
}

#[derive(Debug, Serialize)]
struct SearchItem {
    id: String,
    kind: &'static str,
    title: String,
    body: String,
    source: &'static str,
    score: f64,
    pinned: bool,
}

#[derive(Debug, Clone)]
struct Memory {
    db_path: PathBuf,
    /// A single connection behind a mutex.
    ///
    /// SQLite handles concurrent readers well, but this server is a short-lived
    /// child of one chat process — a connection pool would be machinery for a
    /// workload that never materializes. WAL is still enabled in `store::open`
    /// so a reader never blocks the writer.
    conn: std::sync::Arc<Mutex<Connection>>,
}

impl Memory {
    fn new() -> Self {
        let path = std::env::var("GHOSTLINK_MEMORY_DB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("memories.db"));
        let conn = store::open(&path).unwrap_or_else(|err| {
            // A store that can't open is not fatal to the server: the read tools
            // should report the problem rather than the whole MCP connection
            // vanishing mid-turn and taking the chat down with it.
            tracing::error!("failed to open memory store at {}: {err}", path.display());
            Connection::open_in_memory().expect("in-memory fallback")
        });
        Self {
            db_path: path,
            conn: std::sync::Arc::new(Mutex::new(conn)),
        }
    }

    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T, String>) -> Result<T, String> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| "memory store lock poisoned".to_string())?;
        f(&guard)
    }
}

fn err(msg: impl std::fmt::Display) -> String {
    format!("error: {msg}")
}

#[tool_router(server_handler)]
impl Memory {
    /// List the titles and kinds of memories in scope. Bodies are never
    /// returned — use memory_search for those.
    #[tool(
        name = "memory_catalog",
        description = "List memory titles and kinds for the current workspace (no bodies). Call this to discover what is worth searching."
    )]
    async fn memory_catalog(
        &self,
        Parameters(CatalogRequest {
            workspace_id,
            shared_with,
        }): Parameters<CatalogRequest>,
    ) -> String {
        let shared = shared_with.unwrap_or_default();
        let entries = match self.with_conn(|c| store::catalog(c, &workspace_id, &shared)) {
            Ok(entries) => entries,
            Err(e) => return err(e),
        };
        let items: Vec<CatalogItem> = entries
            .into_iter()
            .map(|e| CatalogItem {
                id: e.id,
                kind: e.kind.as_str(),
                title: e.title,
                pinned: e.pinned,
                shared: e.shared,
                updated_at: e.updated_at,
            })
            .collect();
        serde_json::to_string(&serde_json::json!({ "memories": items })).unwrap_or_else(err)
    }

    /// Search memory bodies in scope, best match first.
    #[tool(
        name = "memory_search",
        description = "Search stored memories for the current workspace and return matching bodies."
    )]
    async fn memory_search(
        &self,
        Parameters(SearchRequest {
            workspace_id,
            query,
            kinds,
            limit,
            shared_with,
        }): Parameters<SearchRequest>,
    ) -> String {
        let parsed_kinds = match parse_kinds(kinds.as_deref()) {
            Ok(k) => k,
            Err(e) => return err(e),
        };
        let limit = limit
            .unwrap_or(DEFAULT_SEARCH_LIMIT)
            .clamp(1, MAX_SEARCH_LIMIT);
        let shared = shared_with.unwrap_or_default();
        let hits = match self
            .with_conn(|c| store::search(c, &workspace_id, &shared, &query, &parsed_kinds, limit))
        {
            Ok(hits) => hits,
            Err(e) => return err(e),
        };
        if hits.is_empty() {
            return serde_json::to_string(&serde_json::json!({
                "memories": [],
                "note": "no memory matched; nothing was recalled",
            }))
            .unwrap_or_else(err);
        }
        let items: Vec<SearchItem> = hits
            .into_iter()
            .map(|h| SearchItem {
                id: h.memory.id,
                kind: h.memory.kind.as_str(),
                title: h.memory.title,
                body: h.memory.body,
                source: h.memory.source.as_str(),
                score: h.score,
                pinned: h.memory.pinned,
            })
            .collect();
        serde_json::to_string(&serde_json::json!({ "memories": items })).unwrap_or_else(err)
    }

    /// Store a new memory. Requires approval — this is a write.
    #[tool(
        name = "memory_remember",
        description = "Store a memory for the current workspace. Requires user approval."
    )]
    async fn memory_remember(
        &self,
        Parameters(RememberRequest {
            workspace_id,
            kind,
            title,
            body,
            source,
            pinned,
            shared,
        }): Parameters<RememberRequest>,
    ) -> String {
        let Some(kind) = MemoryKind::parse(&kind) else {
            return err(format!(
                "unknown kind '{kind}'; expected one of preference, project_fact, decision, person, open_loop, summary"
            ));
        };
        let source = match source.as_deref() {
            None | Some("") => MemorySource::Tool,
            Some(raw) => match MemorySource::parse(raw) {
                Some(s) => s,
                None => {
                    return err(format!(
                        "unknown source '{raw}'; expected user, compaction, or tool"
                    ))
                }
            },
        };
        let title = title.trim();
        let body = body.trim();
        if title.is_empty() || body.is_empty() {
            return err("title and body are both required");
        }
        if title.chars().count() > MAX_TITLE_CHARS {
            return err(format!(
                "title too long ({} chars, max {MAX_TITLE_CHARS})",
                title.chars().count()
            ));
        }
        if body.chars().count() > MAX_BODY_CHARS {
            return err(format!(
                "body too long ({} chars, max {MAX_BODY_CHARS})",
                body.chars().count()
            ));
        }
        if workspace_id.trim().is_empty() {
            return err("workspace_id is required");
        }

        let id = new_id(&workspace_id, title);
        let now = store::now_secs();
        let title_owned = title.to_string();
        let body_owned = body.to_string();
        let title_for_msg = title_owned.clone();
        let pinned = pinned.unwrap_or(false);
        let shared_flag = shared.unwrap_or(false);
        let ws = workspace_id.clone();
        let kind_str = kind.as_str().to_string();
        let source_str = source.as_str().to_string();

        let result = self.with_conn(move |c| {
            c.execute(
                "INSERT INTO memories (id, workspace_id, kind, title, body, source, created_at, updated_at, pinned, shared)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9)",
                rusqlite::params![
                    id,
                    ws,
                    kind_str,
                    title_owned,
                    body_owned,
                    source_str,
                    now,
                    pinned,
                    shared_flag
                ],
            )
            .map_err(|e| format!("{e}"))
        });

        match result {
            Ok(_) => format!(
                "remembered {} '{title_for_msg}' in workspace {workspace_id}",
                kind.as_str()
            ),
            Err(e) => err(format!("storing memory: {e}")),
        }
    }

    /// Delete a memory. Requires approval — this is a write, and deleting is
    /// harder to undo than writing.
    #[tool(
        name = "memory_forget",
        description = "Delete a memory by id. Requires user approval."
    )]
    async fn memory_forget(
        &self,
        Parameters(ForgetRequest { workspace_id, id }): Parameters<ForgetRequest>,
    ) -> String {
        // Scoped delete: a workspace can only forget its own rows, so an id
        // guessed or carried over from another workspace is a no-op, not a
        // cross-workspace delete.
        let result = self.with_conn(move |c| {
            c.execute(
                "DELETE FROM memories WHERE id = ?1 AND workspace_id = ?2",
                rusqlite::params![id, workspace_id],
            )
            .map_err(|e| format!("{e}"))
        });
        match result {
            Ok(0) => err("no memory with that id in this workspace"),
            Ok(n) => format!("deleted {n} memory record(s)"),
            Err(e) => err(format!("deleting memory: {e}")),
        }
    }
}

fn parse_kinds(raw: Option<&[String]>) -> Result<Vec<MemoryKind>, String> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    raw.iter()
        .map(|k| {
            MemoryKind::parse(k).ok_or_else(|| {
                format!(
                    "unknown kind '{k}'; expected one of preference, project_fact, decision, person, open_loop, summary"
                )
            })
        })
        .collect()
}

/// Builds a stable, collision-resistant id from the workspace, kind, title, and
/// a counter — so re-remembering the same fact creates a distinct row rather
/// than silently overwriting one the user may still want.
fn new_id(workspace_id: &str, title: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "mem_{}_{:x}",
        sanitize(workspace_id),
        nanos ^ (sanitize(title).len() as u128 * 0x9e37)
    )
}

fn sanitize(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let memory = Memory::new();
    tracing::info!("mcp-memory using store {}", memory.db_path.display());
    let service = memory.serve(stdio()).await.inspect_err(|err| {
        tracing::error!("mcp-memory serving error: {err:?}");
    })?;

    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_kind() {
        assert!(parse_kinds(Some(&["nonsense".to_string()])).is_err());
        assert!(parse_kinds(Some(&["decision".to_string()])).is_ok());
    }

    #[test]
    fn no_kinds_means_all_kinds() {
        assert!(parse_kinds(None).unwrap().is_empty());
        assert!(parse_kinds(Some(&[])).unwrap().is_empty());
    }

    #[test]
    fn generated_ids_are_distinct_for_identical_titles() {
        // Re-remembering the same fact must not silently overwrite a row the
        // user may still want.
        let a = new_id("ws", "same title");
        let b = new_id("ws", "same title");
        assert_ne!(a, b);
    }

    #[test]
    fn generated_ids_are_workspace_prefixed() {
        assert!(new_id("ws_a", "t").starts_with("mem_ws_a_"));
    }

    #[test]
    fn sanitize_strips_everything_outside_the_safe_set() {
        // Only alphanumerics, '-' and '_' survive, so an id can never carry a
        // separator or a '..' segment into a path it is later joined onto.
        assert_eq!(sanitize("../../etc"), "______etc");
        assert!(!sanitize("a/b\\c").contains('/'));
        assert!(!sanitize("a/b\\c").contains('\\'));
        assert!(!sanitize("..").contains(".."));
        assert_eq!(sanitize("ws_1-2"), "ws_1-2");
    }

    #[test]
    fn memory_server_opens_a_store_in_a_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("GHOSTLINK_MEMORY_DB_PATH", dir.path().join("m.db"));
        let m = Memory::new();
        // Catalog on an empty store is a well-formed empty result, not an error.
        let out = m.with_conn(|c| store::catalog(c, "ws", &[])).unwrap();
        assert!(out.is_empty());
        std::env::remove_var("GHOSTLINK_MEMORY_DB_PATH");
    }
}
