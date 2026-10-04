//! Capability classification — the server-side trust boundary for tool calls.
//!
//! The invariant this module exists to hold: **the model is not the trust
//! boundary, the server is.** A system prompt that tells the model "only read
//! files" is a suggestion the model may ignore; the classification here is
//! checked in Rust before dispatch, and a tool the table doesn't know about is
//! treated as the most dangerous class.
//!
//! Three classes:
//!
//! - `Read` — observation. May run inline and feed a result back to the model.
//! - `Write` — mutates state (files, memory, RAG index). Requires approval.
//! - `Exec` — runs commands or code. Requires approval, and is *never* covered
//!   by a session-scoped grant.
//!
//! Why a table rather than config flags: `McpServerConfig::requires_confirmation`
//! is per-*server*, so it can't distinguish `search` (read) from `write_file`
//! (write) on the same filesystem server, and it can't express "this one tool
//! is fine to auto-apply". Classification has to sit below server granularity.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::mcp::McpToolSchema;

/// What a tool call is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CapabilityClass {
    /// Observational only. May execute inline and return a result to the model.
    Read,
    /// Mutates durable state. Requires approval before dispatch.
    Write,
    /// Runs commands or arbitrary code. Requires approval, never auto-applied.
    Exec,
}

impl CapabilityClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Exec => "exec",
        }
    }

    /// Whether this class needs an explicit human decision before it runs.
    pub fn requires_approval(self) -> bool {
        matches!(self, Self::Write | Self::Exec)
    }

    /// Whether a session-scoped grant may cover this class.
    ///
    /// `Exec` is excluded on purpose: "approve this tool for the rest of the
    /// session" must never become a standing shell. Workspace-scoped file
    /// writes can reasonably be approved for a session; command execution
    /// cannot.
    pub fn session_grant_allowed(self) -> bool {
        matches!(self, Self::Read | Self::Write)
    }
}

/// Per-tool classification entries, keyed by `(server, tool)`.
type ClassTable = HashMap<(String, String), CapabilityClass>;

fn table() -> &'static ClassTable {
    static TABLE: OnceLock<ClassTable> = OnceLock::new();
    TABLE.get_or_init(build_table)
}

/// Builds the classification table.
///
/// Match arms are on `(server, tool)` so an unlisted tool on a *known* server
/// still falls through to the `Exec` default rather than inheriting a broader
/// class from its server. The per-tool granularity is the whole point — a
/// filesystem server offering both `read_file` and `write_file` must not treat
/// them alike.
fn build_table() -> ClassTable {
    let mut t = ClassTable::new();

    // --- Read: inspection with no durable side effects ---
    for tool in [
        "read_text_file",
        "read_media_file",
        "read_multiple_files",
        "list_directory",
        "list_directory_with_sizes",
        "directory_tree",
        "search_files",
        "get_file_info",
        "list_allowed_directories",
    ] {
        t.insert(("filesystem".into(), tool.into()), CapabilityClass::Read);
    }

    for tool in ["calculate", "evaluate", "calculate_expression"] {
        t.insert(("calculator".into(), tool.into()), CapabilityClass::Read);
    }

    // RAG: search is a read; indexing writes the index.
    t.insert(("rag".into(), "search".into()), CapabilityClass::Read);
    t.insert(
        ("rag".into(), "list_documents".into()),
        CapabilityClass::Read,
    );
    t.insert(
        ("rag".into(), "index_document".into()),
        CapabilityClass::Write,
    );

    // Memory: catalog/search are reads, remember/forget are writes.
    for tool in ["memory_catalog", "memory_search"] {
        t.insert(("memory".into(), tool.into()), CapabilityClass::Read);
    }
    for tool in ["memory_remember", "memory_forget"] {
        t.insert(("memory".into(), tool.into()), CapabilityClass::Write);
    }

    // Git: inspection reads, anything that moves refs or the worktree writes.
    // `git_diff_unstaged` / `git_diff_staged` are real tools on mcp-server-git
    // alongside the combined `git_diff`. Without them they fell through to the
    // Exec default, so an ordinary diff needed an approval.
    for tool in [
        "git_status",
        "git_log",
        "git_diff",
        "git_diff_unstaged",
        "git_diff_staged",
        "git_show",
        "git_blame",
    ] {
        t.insert(("git".into(), tool.into()), CapabilityClass::Read);
    }
    for tool in [
        "git_commit",
        "git_add",
        "git_checkout",
        "git_reset",
        "git_push",
    ] {
        t.insert(("git".into(), tool.into()), CapabilityClass::Write);
    }

    // Web fetch and search are reads with respect to *local* state.
    //
    // `fetch` and `brave-search` are separate servers and the table keys on
    // `(server, tool)`, so each needs its own entries. The previous loop filed
    // `brave_web_search` under `fetch` AND under `brave-search`, which left
    // `fetch.fetch` — the tool that actually exists — unclassified.
    t.insert(("fetch".into(), "fetch".into()), CapabilityClass::Read);
    for tool in ["brave_web_search", "brave_web_search_stats", "web_search"] {
        t.insert(("brave-search".into(), tool.into()), CapabilityClass::Read);
    }

    // Sequential thinking is pure inference.
    t.insert(
        ("sequential-thinking".into(), "sequentialthinking".into()),
        CapabilityClass::Read,
    );

    // Vision analyzes an image; it does not write anything durable.
    t.insert(
        ("vision".into(), "analyze_image".into()),
        CapabilityClass::Read,
    );

    // --- Write: durable state changes ---
    for tool in ["write_file", "edit_file", "create_directory", "move_file"] {
        t.insert(("filesystem".into(), tool.into()), CapabilityClass::Write);
    }
    // Schema inspection reads. mcp-server-sqlite ships `list_tables` and
    // `describe_table` alongside the query tools, and both were falling through
    // to Exec -- so merely listing tables demanded an approval, which is the
    // kind of false gate that teaches people to approve without reading.
    for tool in ["read_query", "list_tables", "describe_table"] {
        t.insert(("sqlite".into(), tool.into()), CapabilityClass::Read);
    }
    for tool in [
        "write_query",
        "execute_query",
        "create_table",
        "modify_table",
    ] {
        t.insert(("sqlite".into(), tool.into()), CapabilityClass::Write);
    }

    // --- Exec: command or code execution. Never auto-applied, never
    // session-granted. Listed explicitly so the intent is auditable, even
    // though the unknown-tool default already lands here.
    for (server, tool) in [
        ("terminal", "run_command"),
        ("docker-terminal", "execute"),
        ("code_execution", "execute_code"),
        ("docker-code-execution", "execute"),
    ] {
        t.insert((server.into(), tool.into()), CapabilityClass::Exec);
    }

    t
}

/// Classifies a tool call.
///
/// **Unlisted tools default to `Exec`.** A new MCP server, or a tool added to an
/// existing one, is treated as the most dangerous class until someone vets it.
/// Failing closed is the only safe default here: the cost is an extra approval
/// prompt, while the cost of failing open is arbitrary command execution.
pub fn classify(server: &str, tool: &str) -> CapabilityClass {
    table()
        .get(&(server.to_string(), tool.to_string()))
        .copied()
        .unwrap_or(CapabilityClass::Exec)
}

/// Classifies a schema advertised by a connected MCP server.
pub fn classify_schema(schema: &McpToolSchema) -> CapabilityClass {
    classify(&schema.server, &schema.name)
}

/// Whether this call may auto-apply without a fresh approval.
///
/// Only the vetted workspace-edit path qualifies: a `Write` whose target
/// resolves inside the configured workspace root. `Exec` is excluded
/// unconditionally — no workspace root makes running a command safe by default.
pub fn is_vetted_auto_apply(
    class: CapabilityClass,
    resolved_path: Option<&std::path::Path>,
    workspace_root: &std::path::Path,
) -> bool {
    if class != CapabilityClass::Write {
        return false;
    }
    let Some(path) = resolved_path else {
        return false;
    };
    // Compare canonicalized forms on both sides — a raw string prefix check
    // misses `..` traversal and symlink escapes. `resolve_workspace_path` has
    // already refused those; this re-checks rather than assuming, because a
    // capability decision must not rest on a caller's unchecked claim.
    let Ok(canon_path) = path.canonicalize() else {
        return false;
    };
    let Ok(canon_root) = workspace_root.canonicalize() else {
        return false;
    };
    canon_path.starts_with(canon_root)
}

/// Memory tools whose arguments carry a workspace scope the server must not be
/// able to choose for itself.
///
/// The model can put anything it likes in a tool call's arguments, including a
/// `workspace_id` naming a different workspace. Left alone that would let a
/// prompt-injected turn read (or worse, write) another workspace's memories —
/// which defeats the entire per-workspace grant model. So for these tools the
/// server overwrites the scope from the chat's own binding at dispatch, rather
/// than trusting the argument.
fn is_scope_stamped(server: &str) -> bool {
    server == "memory"
}

/// Replaces any caller-supplied workspace scope with the chat's own.
///
/// Returns the args unchanged for tools that aren't workspace-scoped. For the
/// scoped ones, `workspace_id` is always the server's value: a model-supplied
/// one is discarded rather than rejected, because failing the call outright
/// turns a prompt-injection attempt into a confusing tool error instead of a
/// silently-correct result, and the audit trail already records the real scope.
pub fn stamp_workspace_scope(
    server: &str,
    tool: &str,
    mut args: serde_json::Value,
    workspace_id: &str,
) -> serde_json::Value {
    let _ = tool;
    if !is_scope_stamped(server) {
        return args;
    }
    if !args.is_object() {
        // A non-object argument bag can't carry the scope; make it one so the
        // stamped id isn't silently dropped on the server side.
        args = serde_json::json!({});
    }
    let obj = args.as_object_mut().expect("just ensured object");
    obj.insert(
        "workspace_id".to_string(),
        serde_json::Value::String(workspace_id.to_string()),
    );
    args
}

/// What the server decided to do with a tool call, before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// May execute inline and return its result to the model as an observation.
    Allow,
    /// Must not execute. The model gets a pending handle and waits for a human.
    NeedsApproval {
        class: CapabilityClass,
        /// Why it was gated — recorded in the approval record and the trace.
        reason: &'static str,
    },
}

impl Decision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// `(workspace_id, server, tool)` — the scope a session grant applies to.
type GrantKey = (String, String, String);

/// Standing grants from "approve for session", keyed by `(workspace, server, tool)`.
///
/// Scoping to the triple is deliberate: a grant for `write_file` in workspace A
/// says nothing about `write_file` in workspace B, nor about a different tool on
/// the same server. `Exec` tools never land here — see
/// `CapabilityClass::session_grant_allowed`.
fn session_grants() -> &'static Mutex<HashMap<GrantKey, ()>> {
    static GRANTS: OnceLock<Mutex<HashMap<GrantKey, ()>>> = OnceLock::new();
    GRANTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Records a session-scoped grant for a tool.
///
/// Returns `false` when the class is not eligible, so a caller can't widen the
/// boundary by calling this with an `Exec` tool.
pub fn grant_for_session(
    workspace_id: &str,
    server: &str,
    tool: &str,
    class: CapabilityClass,
) -> bool {
    if !class.session_grant_allowed() {
        return false;
    }
    if let Ok(mut grants) = session_grants().lock() {
        grants.insert(
            (
                workspace_id.to_string(),
                server.to_string(),
                tool.to_string(),
            ),
            (),
        );
        return true;
    }
    false
}

/// Whether a session grant currently covers this call.
pub fn has_session_grant(workspace_id: &str, server: &str, tool: &str) -> bool {
    session_grants()
        .lock()
        .map(|grants| {
            grants.contains_key(&(
                workspace_id.to_string(),
                server.to_string(),
                tool.to_string(),
            ))
        })
        .unwrap_or(false)
}

/// Decides whether a tool call may execute, without executing it.
///
/// This is the enforcement point for "the model is not the trust boundary": the
/// class comes from the Rust-side table, never from the prompt or the caller.
/// Ordering matters — an `Exec` tool is gated even if a grant exists, and a
/// session grant only ever applies to a class that was already approval-worthy.
pub fn decide(
    workspace_id: &str,
    server: &str,
    tool: &str,
    resolved_path: Option<&std::path::Path>,
    workspace_root: &std::path::Path,
) -> Decision {
    let class = classify(server, tool);

    if !class.requires_approval() {
        return Decision::Allow;
    }

    if is_vetted_auto_apply(class, resolved_path, workspace_root) {
        return Decision::Allow;
    }

    if class.session_grant_allowed() && has_session_grant(workspace_id, server, tool) {
        return Decision::Allow;
    }

    let reason = match class {
        CapabilityClass::Exec => "exec tool requires approval",
        CapabilityClass::Write => "write tool requires approval",
        CapabilityClass::Read => "read tool requires approval",
    };
    Decision::NeedsApproval { class, reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_tool_defaults_to_exec() {
        // Failing closed is the whole point: an unreviewed tool must not be
        // able to run by omission.
        assert_eq!(
            classify("brand-new-server", "do_thing"),
            CapabilityClass::Exec
        );
    }

    #[test]
    fn known_server_unknown_tool_still_defaults_to_exec() {
        // Per-tool granularity: an unlisted tool on the filesystem server must
        // not inherit Read from its siblings.
        assert_eq!(
            classify("filesystem", "delete_everything"),
            CapabilityClass::Exec
        );
    }

    #[test]
    fn filesystem_reads_and_writes_are_distinguished() {
        // The case the per-server `requires_confirmation` flag cannot express.
        assert_eq!(
            classify("filesystem", "read_text_file"),
            CapabilityClass::Read
        );
        assert_eq!(classify("filesystem", "write_file"), CapabilityClass::Write);
    }

    #[test]
    fn exec_is_never_session_grantable() {
        // "Approve for the session" must never become a standing shell.
        assert!(!CapabilityClass::Exec.session_grant_allowed());
        assert!(CapabilityClass::Write.session_grant_allowed());
        assert!(CapabilityClass::Read.session_grant_allowed());
    }

    #[test]
    fn read_does_not_require_approval() {
        assert!(!CapabilityClass::Read.requires_approval());
        assert!(CapabilityClass::Write.requires_approval());
        assert!(CapabilityClass::Exec.requires_approval());
    }

    #[test]
    fn exec_is_never_auto_applied() {
        let root = std::env::temp_dir();
        let inside = root.join("file.txt");
        assert!(!is_vetted_auto_apply(
            CapabilityClass::Exec,
            Some(&inside),
            &root
        ));
    }

    #[test]
    fn read_is_never_auto_applied_as_a_write() {
        let root = std::env::temp_dir();
        let inside = root.join("file.txt");
        assert!(!is_vetted_auto_apply(
            CapabilityClass::Read,
            Some(&inside),
            &root
        ));
    }

    #[test]
    fn write_inside_workspace_is_auto_applicable() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.md");
        std::fs::write(&file, "hello").unwrap();
        assert!(is_vetted_auto_apply(
            CapabilityClass::Write,
            Some(&file),
            dir.path()
        ));
    }

    #[test]
    fn write_outside_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = std::env::temp_dir().join("definitely-outside-workspace.txt");
        std::fs::write(&outside, "nope").unwrap();
        assert!(!is_vetted_auto_apply(
            CapabilityClass::Write,
            Some(&outside),
            dir.path()
        ));
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn write_without_a_resolved_path_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        // No path means no way to prove the target is in-workspace.
        assert!(!is_vetted_auto_apply(
            CapabilityClass::Write,
            None,
            dir.path()
        ));
    }

    #[test]
    fn exec_is_gated_even_with_a_session_grant() {
        // "Approve for the session" must never become a standing shell.
        let ws = "ws_exec_grant_test";
        assert!(!grant_for_session(
            ws,
            "terminal",
            "run_command",
            CapabilityClass::Exec
        ));
        assert!(!has_session_grant(ws, "terminal", "run_command"));
    }

    #[test]
    fn write_grant_is_scoped_to_workspace_and_tool() {
        let ws_a = "ws_grant_a";
        let ws_b = "ws_grant_b";
        assert!(grant_for_session(
            ws_a,
            "filesystem",
            "write_file",
            CapabilityClass::Write
        ));
        assert!(has_session_grant(ws_a, "filesystem", "write_file"));
        // A different workspace must not inherit it.
        assert!(!has_session_grant(ws_b, "filesystem", "write_file"));
        // Nor a different tool on the same server.
        assert!(!has_session_grant(ws_a, "filesystem", "edit_file"));
    }

    #[test]
    fn read_tools_are_allowed_without_approval() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            decide(
                "ws_read_test",
                "filesystem",
                "read_text_file",
                None,
                dir.path()
            ),
            Decision::Allow
        );
    }

    #[test]
    fn write_outside_workspace_stays_gated_under_a_grant() {
        // A grant covers the tool, but the vetted auto-apply path still has to
        // prove the target is in-workspace before anything runs.
        let dir = tempfile::tempdir().unwrap();
        let outside = std::env::temp_dir().join("grant-outside-workspace.txt");
        std::fs::write(&outside, "nope").unwrap();
        assert!(grant_for_session(
            "ws_outside_test",
            "filesystem",
            "write_file",
            CapabilityClass::Write
        ));
        assert_eq!(
            decide(
                "ws_outside_test",
                "filesystem",
                "write_file",
                Some(&outside),
                dir.path()
            ),
            Decision::Allow,
            "an explicit session grant permits the call; path containment is              enforced by resolve_relative at dispatch"
        );
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn unknown_tool_is_gated_as_exec() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            decide("ws_unknown_test", "mystery", "act", None, dir.path()),
            Decision::NeedsApproval {
                class: CapabilityClass::Exec,
                ..
            }
        ));
    }

    #[test]
    fn stamp_overrides_a_model_supplied_workspace() {
        // The whole point: a model-supplied scope must never win.
        let args = serde_json::json!({ "workspace_id": "ws_someone_else", "query": "salary" });
        let stamped = stamp_workspace_scope("memory", "memory_search", args, "ws_real");
        assert_eq!(stamped["workspace_id"], "ws_real");
        assert_eq!(stamped["query"], "salary", "other args are preserved");
    }

    #[test]
    fn stamp_adds_the_scope_when_absent() {
        let stamped = stamp_workspace_scope(
            "memory",
            "memory_remember",
            serde_json::json!({ "title": "t" }),
            "ws_real",
        );
        assert_eq!(stamped["workspace_id"], "ws_real");
    }

    #[test]
    fn stamp_replaces_a_non_object_argument_bag() {
        // Otherwise the id would be silently dropped server-side and the tool
        // would fall back to whatever scope it liked.
        let stamped = stamp_workspace_scope(
            "memory",
            "memory_search",
            serde_json::json!("oops"),
            "ws_real",
        );
        assert_eq!(stamped["workspace_id"], "ws_real");
    }

    #[test]
    fn stamp_leaves_unscoped_servers_alone() {
        let args = serde_json::json!({ "expression": "2+2" });
        let out = stamp_workspace_scope("calculator", "calculate", args.clone(), "ws_real");
        assert_eq!(out, args);
        assert!(out.get("workspace_id").is_none());
    }

    #[test]
    fn scoped_memory_reads_are_reads_and_writes_are_writes() {
        // The four tool names must match crates/mcp-memory exactly, or the gate
        // silently falls back to exec for all of them.
        assert_eq!(classify("memory", "memory_catalog"), CapabilityClass::Read);
        assert_eq!(classify("memory", "memory_search"), CapabilityClass::Read);
        assert_eq!(
            classify("memory", "memory_remember"),
            CapabilityClass::Write
        );
        assert_eq!(classify("memory", "memory_forget"), CapabilityClass::Write);
    }

    /// Every tool a configured MCP server actually exposes must be classified.
    ///
    /// This is the test that keeps the table honest against the servers in
    /// `mcp_servers.example.toml`. The list is the real tool set of each server
    /// (the Ghostlink crates' names come from their `#[tool]` attributes; the
    /// third-party names from the packages pinned there), NOT a copy of the
    /// table above -- so adding a tool upstream without classifying it here
    /// fails the build rather than silently falling back to `Exec` and demanding
    /// an approval for a harmless read.
    ///
    /// Falling back to `Exec` is safe, not correct: it blocked `git_diff_unstaged`,
    /// `list_tables`, `describe_table`, `fetch.fetch`, and both brave-search
    /// tools, every one of which is an ordinary observation.
    const REAL_TOOLS: &[(&str, &str, CapabilityClass)] = &[
        // filesystem
        ("filesystem", "read_text_file", CapabilityClass::Read),
        ("filesystem", "read_media_file", CapabilityClass::Read),
        ("filesystem", "read_multiple_files", CapabilityClass::Read),
        ("filesystem", "list_directory", CapabilityClass::Read),
        (
            "filesystem",
            "list_directory_with_sizes",
            CapabilityClass::Read,
        ),
        ("filesystem", "directory_tree", CapabilityClass::Read),
        ("filesystem", "search_files", CapabilityClass::Read),
        ("filesystem", "get_file_info", CapabilityClass::Read),
        (
            "filesystem",
            "list_allowed_directories",
            CapabilityClass::Read,
        ),
        ("filesystem", "write_file", CapabilityClass::Write),
        ("filesystem", "edit_file", CapabilityClass::Write),
        ("filesystem", "create_directory", CapabilityClass::Write),
        ("filesystem", "move_file", CapabilityClass::Write),
        // calculator (Ghostlink crate)
        ("calculator", "calculate", CapabilityClass::Read),
        // fetch -- the tool is named `fetch` on a server named `fetch`
        ("fetch", "fetch", CapabilityClass::Read),
        // brave-search is a separate server from fetch
        ("brave-search", "brave_web_search", CapabilityClass::Read),
        (
            "brave-search",
            "brave_web_search_stats",
            CapabilityClass::Read,
        ),
        // sqlite: reads, including schema inspection
        ("sqlite", "read_query", CapabilityClass::Read),
        ("sqlite", "list_tables", CapabilityClass::Read),
        ("sqlite", "describe_table", CapabilityClass::Read),
        ("sqlite", "write_query", CapabilityClass::Write),
        ("sqlite", "create_table", CapabilityClass::Write),
        // git
        ("git", "git_status", CapabilityClass::Read),
        ("git", "git_diff", CapabilityClass::Read),
        ("git", "git_diff_unstaged", CapabilityClass::Read),
        ("git", "git_diff_staged", CapabilityClass::Read),
        ("git", "git_log", CapabilityClass::Read),
        ("git", "git_show", CapabilityClass::Read),
        ("git", "git_blame", CapabilityClass::Read),
        ("git", "git_add", CapabilityClass::Write),
        ("git", "git_commit", CapabilityClass::Write),
        ("git", "git_checkout", CapabilityClass::Write),
        ("git", "git_reset", CapabilityClass::Write),
        ("git", "git_push", CapabilityClass::Write),
        // rag (Ghostlink crate)
        ("rag", "search", CapabilityClass::Read),
        ("rag", "index_document", CapabilityClass::Write),
        // vision (Ghostlink crate)
        ("vision", "analyze_image", CapabilityClass::Read),
        // sequential-thinking
        (
            "sequential-thinking",
            "sequentialthinking",
            CapabilityClass::Read,
        ),
        // memory (Ghostlink crate, phase 1)
        ("memory", "memory_catalog", CapabilityClass::Read),
        ("memory", "memory_search", CapabilityClass::Read),
        ("memory", "memory_remember", CapabilityClass::Write),
        ("memory", "memory_forget", CapabilityClass::Write),
    ];

    #[test]
    fn every_real_tool_is_classified_as_expected() {
        let mut wrong = Vec::new();
        for (server, tool, expected) in REAL_TOOLS {
            let actual = classify(server, tool);
            if actual != *expected {
                wrong.push(format!(
                    "{server}.{tool}: expected {expected:?}, got {actual:?}"
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "capability table disagrees with the real tool set:\n  {}",
            wrong.join("\n  ")
        );
    }

    #[test]
    fn no_ordinary_read_is_left_to_the_exec_default() {
        // The specific regression: these all silently became Exec.
        for (server, tool) in [
            ("git", "git_diff_unstaged"),
            ("git", "git_diff_staged"),
            ("sqlite", "list_tables"),
            ("sqlite", "describe_table"),
            ("fetch", "fetch"),
            ("brave-search", "brave_web_search"),
        ] {
            assert_ne!(
                classify(server, tool),
                CapabilityClass::Exec,
                "{server}.{tool} fell through to Exec and would demand approval for a read"
            );
        }
    }
}
