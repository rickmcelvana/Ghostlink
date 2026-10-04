//! The approval queue: gated tool calls waiting on a human decision.
//!
//! Phase 3 of the local assistant. When the capability gate classifies a call as
//! `write` or `exec`, the call is *not* run. It is recorded here as a pending
//! action, and the model is handed a handle it can talk about. The turn finishes
//! normally instead of stalling, which is the difference between the previous
//! blocking flow and this one.
//!
//! Design points that are load-bearing rather than incidental:
//!
//! - **Persistent.** A pending write is a promise to the user that something is
//!   waiting. Losing it on restart would silently drop the request, so the queue
//!   is a JSON file written on every mutation — the same shape `sessions.json`
//!   and `api_keys.json` already use in this crate, deliberately chosen over
//!   adding SQLite to the server binary just for a queue of tens of rows.
//! - **Preview, not payload.** [`build_preview`] extracts a short, bounded
//!   description of what the call *would* do — a target path, a SQL statement, a
//!   command line — and never serializes the whole argument bag, which is where
//!   credentials would turn up.
//! - **Scoped.** Every action carries its `workspace_id` and is only listable or
//!   decidable by that workspace, so a chat bound to workspace A cannot see or
//!   approve workspace B's queued writes.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::capability::CapabilityClass;

/// Longest preview retained. A preview is read by a human deciding whether to
/// allow an action; past a few hundred characters it is a file dump, not a
/// preview, and the arguments are still available on the action itself.
const MAX_PREVIEW_CHARS: usize = 400;

/// Where a queued action stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    /// Waiting on a human.
    Pending,
    /// A human allowed it once.
    Approved,
    /// A human edited the arguments, then allowed it.
    Edited,
    /// A human refused.
    Denied,
    /// A human allowed it for the rest of the session.
    ApprovedForSession,
}

impl ApprovalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Edited => "edited",
            Self::Denied => "denied",
            Self::ApprovedForSession => "approved_for_session",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "edited" => Some(Self::Edited),
            "denied" => Some(Self::Denied),
            "approved_for_session" => Some(Self::ApprovedForSession),
            _ => None,
        }
    }

    /// Whether the action still needs a decision.
    pub fn is_pending(self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// One gated call awaiting a decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingAction {
    pub id: String,
    pub workspace_id: String,
    /// The chat turn that requested it, for display and correlation.
    pub turn_id: String,
    pub tool: String,
    pub server: String,
    pub class: String,
    /// The full arguments, retained so an approved action can actually run.
    ///
    /// Never logged, never traced, and never returned by the list endpoint —
    /// see `to_summary` for what leaves the process.
    #[serde(default)]
    pub args: serde_json::Value,
    /// Short human-readable description of the intended effect.
    pub preview: String,
    pub status: ApprovalStatus,
    pub created_at: i64,
    #[serde(default)]
    pub resolved_at: Option<i64>,
    /// The tool's output once approved and executed.
    #[serde(default)]
    pub result: Option<String>,
    /// Whether the execution failed, when it ran.
    #[serde(default)]
    pub result_failed: Option<bool>,
}

/// The list-facing view of an action: everything a tray needs, no arguments.
///
/// `args` is deliberately absent from this type rather than merely omitted when
/// serializing, so adding it later has to be a deliberate change to a struct the
/// list endpoint returns.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionSummary {
    pub id: String,
    pub workspace_id: String,
    pub turn_id: String,
    pub tool: String,
    pub server: String,
    pub class: String,
    pub preview: String,
    pub status: &'static str,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
}

impl PendingAction {
    pub fn to_summary(&self) -> ActionSummary {
        ActionSummary {
            id: self.id.clone(),
            workspace_id: self.workspace_id.clone(),
            turn_id: self.turn_id.clone(),
            tool: self.tool.clone(),
            server: self.server.clone(),
            class: self.class.clone(),
            preview: self.preview.clone(),
            status: self.status.as_str(),
            created_at: self.created_at,
            resolved_at: self.resolved_at,
        }
    }

    /// The observation handed back to the model in place of a result.
    ///
    /// Phrased so the model reports the pending state rather than claiming the
    /// action happened: the common failure mode here is a model reading "queued"
    /// and telling the user the file was written.
    pub fn model_observation(&self) -> String {
        format!(
            "Approval required: `{tool}` (class={class}) was NOT run. It is queued for a \
             human decision as approval `{id}`. Intended effect: {preview}. Tell the user it \
             needs their approval and continue without its result — do not claim it completed.",
            tool = self.tool,
            class = self.class,
            id = self.id,
            preview = self.preview
        )
    }
}

/// Extracts a short description of what a call would do.
///
/// Reads only specific, named argument fields per server. A generic
/// "render the args as JSON" fallback is deliberately absent: the argument bag
/// is exactly where a credential would appear, and a preview is not worth that
/// risk. An unrecognized shape yields a neutral description rather than the
/// arguments themselves.
pub fn build_preview(server: &str, tool: &str, args: &serde_json::Value) -> String {
    let s = || -> String {
        args.get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let described = match server {
        "filesystem" => match tool {
            "write_file" | "edit_file" | "read_text_file" | "read_media_file" | "move_file" => {
                let target = s();
                if target.is_empty() {
                    None
                } else {
                    Some(format!("{tool} on {target}"))
                }
            }
            _ => None,
        },
        "memory" => {
            let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if title.is_empty() && kind.is_empty() {
                None
            } else {
                Some(format!("{kind} memory {title:?}"))
            }
        }
        "sqlite" => {
            // The statement is the point of the preview for a SQL write, and
            // table/column names are not secrets.
            let stmt = args
                .get("query")
                .or_else(|| args.get("sql"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if stmt.is_empty() {
                None
            } else {
                Some(stmt.to_string())
            }
        }
        "rag" => args
            .get("source")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(|v| format!("index {v}")),
        _ => None,
    };
    truncate(described.unwrap_or_else(|| format!("invoke {server}/{tool}")))
}

/// Truncates on a char boundary, marking that it happened.
fn truncate(mut text: String) -> String {
    if text.chars().count() <= MAX_PREVIEW_CHARS {
        return text;
    }
    let kept: String = text.chars().take(MAX_PREVIEW_CHARS).collect();
    text = format!("{kept}…");
    text
}

/// The on-disk queue.
#[derive(Debug, Default, Serialize, Deserialize)]
struct QueueFile {
    #[serde(default)]
    actions: Vec<PendingAction>,
}

/// Persistent, workspace-scoped approval queue.
#[derive(Debug)]
pub struct ApprovalStore {
    path: PathBuf,
    /// Insertion-ordered; the file is rewritten whole on each mutation, which at
    /// tens of pending rows is far cheaper than the WAL machinery SQLite would
    /// bring for the same data.
    actions: Mutex<Vec<PendingAction>>,
}

impl ApprovalStore {
    /// Opens the store at `path`, creating it if absent.
    ///
    /// A corrupt or unreadable file is *not* fatal: the worst outcome of a
    /// malformed queue should be an empty tray the user re-requests from, not a
    /// server that won't start. The old file is left in place for inspection
    /// rather than overwritten, so nothing is destroyed by a parse failure.
    pub fn open(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        let actions = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<QueueFile>(&raw).ok())
            .map(|q| q.actions)
            .unwrap_or_default();
        Self {
            path,
            actions: Mutex::new(actions),
        }
    }

    fn persist(&self, actions: &[PendingAction]) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = QueueFile {
            actions: actions.to_vec(),
        };
        match serde_json::to_string_pretty(&file) {
            Ok(json) => {
                if let Err(err) = std::fs::write(&self.path, json) {
                    tracing::warn!("failed to persist approval queue: {err}");
                }
            }
            Err(err) => tracing::warn!("failed to serialize approval queue: {err}"),
        }
    }

    /// Records a gated call and returns the queued action.
    pub fn enqueue(
        &self,
        workspace_id: &str,
        turn_id: &str,
        server: &str,
        tool: &str,
        class: CapabilityClass,
        args: serde_json::Value,
    ) -> PendingAction {
        let action = PendingAction {
            id: uuid::Uuid::new_v4().to_string(),
            workspace_id: workspace_id.to_string(),
            turn_id: turn_id.to_string(),
            tool: tool.to_string(),
            server: server.to_string(),
            class: class.as_str().to_string(),
            preview: build_preview(server, tool, &args),
            args,
            status: ApprovalStatus::Pending,
            created_at: now_secs(),
            resolved_at: None,
            result: None,
            result_failed: None,
        };
        let mut guard = self.actions.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(action.clone());
        self.persist(&guard);
        action
    }

    /// Pending actions for one workspace, oldest first.
    pub fn list_pending(&self, workspace_id: &str) -> Vec<PendingAction> {
        let guard = self.actions.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .iter()
            .filter(|a| a.workspace_id == workspace_id && a.status.is_pending())
            .cloned()
            .collect()
    }

    /// Every action for one workspace, including resolved ones.
    pub fn list_all(&self, workspace_id: &str) -> Vec<PendingAction> {
        let guard = self.actions.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .iter()
            .filter(|a| a.workspace_id == workspace_id)
            .cloned()
            .collect()
    }

    /// Resolves an action, scoped to the workspace that owns it.
    ///
    /// Returns `None` when no such action exists *in that workspace* — which is
    /// deliberately indistinguishable from "no such action": a caller must not be
    /// able to probe for another workspace's ids.
    pub fn resolve(
        &self,
        workspace_id: &str,
        id: &str,
        status: ApprovalStatus,
        edited_args: Option<serde_json::Value>,
    ) -> Option<PendingAction> {
        let mut guard = self.actions.lock().unwrap_or_else(|e| e.into_inner());
        let action = guard
            .iter_mut()
            .find(|a| a.id == id && a.workspace_id == workspace_id)?;
        if !action.status.is_pending() {
            // Already decided. Returning the existing record (rather than
            // erroring) lets a duplicate GUI click be harmless.
            return Some(action.clone());
        }
        action.status = status;
        action.resolved_at = Some(now_secs());
        if let Some(new_args) = edited_args {
            action.preview = build_preview(&action.server, &action.tool, &new_args);
            action.args = new_args;
        }
        let resolved = action.clone();
        self.persist(&guard);
        Some(resolved)
    }

    /// Records an approved action's execution result.
    pub fn record_result(&self, workspace_id: &str, id: &str, result: String, failed: bool) {
        let mut guard = self.actions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(action) = guard
            .iter_mut()
            .find(|a| a.id == id && a.workspace_id == workspace_id)
        {
            action.result = Some(result);
            action.result_failed = Some(failed);
            self.persist(&guard);
        }
    }

    /// Drops resolved actions older than `max_age_secs`, keeping pending ones.
    ///
    /// Pending rows are never pruned regardless of age: an old unanswered
    /// request is still a promise, and silently deleting it would lose the user's
    /// queued work.
    pub fn prune_resolved(&self, max_age_secs: i64) -> usize {
        let cutoff = now_secs() - max_age_secs;
        let mut guard = self.actions.lock().unwrap_or_else(|e| e.into_inner());
        let before = guard.len();
        guard
            .retain(|a| a.status.is_pending() || a.resolved_at.map(|t| t > cutoff).unwrap_or(true));
        let removed = before - guard.len();
        if removed > 0 {
            self.persist(&guard);
        }
        removed
    }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Resolves the workspace an approval request is acting in.
pub fn requested_workspace(requested: Option<&str>) -> String {
    match requested {
        Some(id) => crate::workspace::sanitize_id(id),
        None => crate::active_workspace().id().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store(dir: &tempfile::TempDir) -> ApprovalStore {
        ApprovalStore::open(dir.path().join("approvals.json"))
    }

    #[test]
    fn preview_names_the_target_file_not_the_arguments() {
        let args = json!({ "path": "src/main.rs", "content": "SECRET_TOKEN=abc123" });
        let preview = build_preview("filesystem", "write_file", &args);
        assert_eq!(preview, "write_file on src/main.rs");
        assert!(!preview.contains("SECRET_TOKEN"));
    }

    #[test]
    fn preview_never_leaks_unknown_argument_values() {
        // An unrecognized shape must not fall back to dumping the bag: that's
        // where credentials live.
        let args = json!({ "api_key": "sk-live-1234", "headers": { "auth": "bearer" } });
        let preview = build_preview("mystery-server", "do_thing", &args);
        assert!(!preview.contains("sk-live-1234"));
        assert!(!preview.contains("bearer"));
        assert_eq!(preview, "invoke mystery-server/do_thing");
    }

    #[test]
    fn preview_includes_sql_statement() {
        let args = json!({ "query": "DELETE FROM sessions WHERE id = 7" });
        assert_eq!(
            build_preview("sqlite", "write_query", &args),
            "DELETE FROM sessions WHERE id = 7"
        );
    }

    #[test]
    fn preview_is_truncated_loudly() {
        let long = "x".repeat(2000);
        let args = json!({ "query": long });
        let preview = build_preview("sqlite", "write_query", &args);
        assert!(preview.chars().count() <= MAX_PREVIEW_CHARS + 1);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn observation_does_not_claim_the_action_ran() {
        let s = store(&tempfile::tempdir().unwrap());
        let action = s.enqueue(
            "ws_a",
            "turn-1",
            "filesystem",
            "write_file",
            CapabilityClass::Write,
            json!({ "path": "a.txt" }),
        );
        let obs = action.model_observation();
        assert!(obs.contains("NOT run"));
        assert!(obs.contains(&action.id));
        assert!(obs.contains("needs their approval"));
    }

    #[test]
    fn enqueued_action_defaults_to_pending_and_is_listed() {
        let s = store(&tempfile::tempdir().unwrap());
        s.enqueue(
            "ws_a",
            "t",
            "filesystem",
            "write_file",
            CapabilityClass::Write,
            json!({}),
        );
        let pending = s.list_pending("ws_a");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, ApprovalStatus::Pending);
    }

    #[test]
    fn queue_is_scoped_per_workspace() {
        let s = store(&tempfile::tempdir().unwrap());
        s.enqueue(
            "ws_a",
            "t",
            "filesystem",
            "write_file",
            CapabilityClass::Write,
            json!({}),
        );
        assert!(s.list_pending("ws_b").is_empty());
        assert_eq!(s.list_pending("ws_a").len(), 1);
    }

    #[test]
    fn another_workspace_cannot_resolve_or_probe_an_action() {
        let s = store(&tempfile::tempdir().unwrap());
        let action = s.enqueue(
            "ws_a",
            "t",
            "filesystem",
            "write_file",
            CapabilityClass::Write,
            json!({}),
        );
        // ws_b resolving ws_a's id must fail indistinguishably from a bogus id.
        assert!(s
            .resolve("ws_b", &action.id, ApprovalStatus::Approved, None)
            .is_none());
        assert!(s
            .resolve("ws_b", "does-not-exist", ApprovalStatus::Approved, None)
            .is_none());
        // ...and ws_a's action is untouched.
        assert_eq!(s.list_pending("ws_a")[0].status, ApprovalStatus::Pending);
    }

    #[test]
    fn resolve_records_status_and_timestamp() {
        let s = store(&tempfile::tempdir().unwrap());
        let action = s.enqueue(
            "ws_a",
            "t",
            "filesystem",
            "write_file",
            CapabilityClass::Write,
            json!({}),
        );
        let resolved = s
            .resolve("ws_a", &action.id, ApprovalStatus::Denied, None)
            .unwrap();
        assert_eq!(resolved.status, ApprovalStatus::Denied);
        assert!(resolved.resolved_at.is_some());
        assert!(s.list_pending("ws_a").is_empty());
    }

    #[test]
    fn editing_replaces_args_and_rebuilds_the_preview() {
        let s = store(&tempfile::tempdir().unwrap());
        let action = s.enqueue(
            "ws_a",
            "t",
            "filesystem",
            "write_file",
            CapabilityClass::Write,
            json!({ "path": "danger.txt" }),
        );
        let edited = s
            .resolve(
                "ws_a",
                &action.id,
                ApprovalStatus::Edited,
                Some(json!({ "path": "safe.txt" })),
            )
            .unwrap();
        assert_eq!(edited.preview, "write_file on safe.txt");
        assert_eq!(edited.args["path"], "safe.txt");
    }

    #[test]
    fn double_resolve_is_idempotent() {
        // A duplicate GUI click must not flip a denial into an approval.
        let s = store(&tempfile::tempdir().unwrap());
        let action = s.enqueue(
            "ws_a",
            "t",
            "filesystem",
            "write_file",
            CapabilityClass::Write,
            json!({}),
        );
        let first = s
            .resolve("ws_a", &action.id, ApprovalStatus::Denied, None)
            .unwrap();
        let second = s
            .resolve("ws_a", &action.id, ApprovalStatus::Approved, None)
            .unwrap();
        assert_eq!(first.status, second.status);
        assert_eq!(second.status, ApprovalStatus::Denied);
    }

    #[test]
    fn queue_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approvals.json");
        {
            let s = ApprovalStore::open(&path);
            s.enqueue(
                "ws_a",
                "t",
                "filesystem",
                "write_file",
                CapabilityClass::Write,
                json!({ "path": "x" }),
            );
        }
        let reopened = ApprovalStore::open(&path);
        let pending = reopened.list_pending("ws_a");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].preview, "write_file on x");
        assert!(
            pending[0].args["path"] == "x",
            "args must survive for execution"
        );
    }

    #[test]
    fn corrupt_queue_file_does_not_panic_and_does_not_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approvals.json");
        std::fs::write(&path, "{not json").unwrap();
        let s = ApprovalStore::open(&path);
        assert!(s.list_pending("ws_a").is_empty());
        // The bad file is left intact for inspection.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
    }

    #[test]
    fn prune_never_drops_a_pending_action() {
        let s = store(&tempfile::tempdir().unwrap());
        let old = s.enqueue(
            "ws_a",
            "t",
            "filesystem",
            "write_file",
            CapabilityClass::Write,
            json!({}),
        );
        let done = s.enqueue(
            "ws_a",
            "t",
            "filesystem",
            "write_file",
            CapabilityClass::Write,
            json!({}),
        );
        s.resolve("ws_a", &done.id, ApprovalStatus::Denied, None);
        // Cutoff far in the future: everything resolved is "old".
        s.prune_resolved(i64::MAX / 2);
        let all = s.list_all("ws_a");
        assert!(
            all.iter().any(|a| a.id == old.id),
            "pending action must survive"
        );
    }

    #[test]
    fn summary_omits_arguments() {
        let s = store(&tempfile::tempdir().unwrap());
        let action = s.enqueue(
            "ws_a",
            "t",
            "filesystem",
            "write_file",
            CapabilityClass::Write,
            json!({ "path": "a.txt", "content": "secret" }),
        );
        let json = serde_json::to_string(&action.to_summary()).unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("args"));
    }

    #[test]
    fn status_strings_round_trip() {
        for status in [
            ApprovalStatus::Pending,
            ApprovalStatus::Approved,
            ApprovalStatus::Edited,
            ApprovalStatus::Denied,
            ApprovalStatus::ApprovedForSession,
        ] {
            assert_eq!(ApprovalStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(ApprovalStatus::parse("garbage"), None);
    }
}
