//! Workspace identity — the scoping primitive every capability check hangs off.
//!
//! Ghostlink's chat path historically had a single implicit workspace: the
//! process's `GHOSTLINK_WORKSPACE_ROOT` (defaulting to the launch directory).
//! The local assistant layer needs grants, memories, RAG indexes, and approvals
//! to be scoped per workspace, so a chat bound to workspace A can never see
//! workspace B's files or data.
//!
//! This module derives a stable id from a canonicalized workspace root. The id
//! is a short hash rather than the raw path so it can appear in filenames,
//! SQLite keys, and trace records without leaking the operator's directory
//! layout, and so it stays bounded in length on Windows where a deep path
//! easily exceeds a practical filename budget.

use std::path::{Path, PathBuf};

/// How a workspace id was resolved. Recorded alongside stored memories,
/// schedules, and approvals so a stale id can be told apart from a wrong one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceSource {
    /// Client supplied the id explicitly (GUI, API, or a schedule run).
    Explicit,
    /// Derived from the process's configured workspace root.
    ConfiguredRoot,
}

impl WorkspaceSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::ConfiguredRoot => "configured_root",
        }
    }
}

/// The resolved workspace a request is bound to.
///
/// Deliberately cheap to clone and `Copy`-comparable: it is threaded through the
/// engine paths next to the existing `history` parameter, so it needs to be
/// boring to pass around.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceId {
    id: String,
    root: PathBuf,
    source: WorkspaceSource,
}

impl WorkspaceId {
    /// Binds a request to an explicit id and root.
    pub fn explicit(id: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            id: sanitize_id(&id.into()),
            root: root.into(),
            source: WorkspaceSource::Explicit,
        }
    }

    /// Binds a request to the process's configured workspace root, deriving the
    /// id from its canonicalized path. Falls back to the raw path when
    /// canonicalization fails (the root may not exist yet, e.g. a fresh
    /// workspace) so a first-run chat is never rejected for a missing directory.
    pub fn from_root(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        let id_source = root.canonicalize().unwrap_or_else(|_| root.clone());
        Self {
            id: derive_id(&id_source),
            root,
            source: WorkspaceSource::ConfiguredRoot,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn source(&self) -> WorkspaceSource {
        self.source
    }
}

/// Resolves a client-supplied relative path against a workspace root and
/// rejects anything that escapes it.
///
/// Shared by `WorkspaceId::resolve_relative` and the API layer's workspace file
/// routes so the sandbox has exactly one implementation to audit.
pub fn resolve_within(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim_start_matches(['/', '\\']);
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("workspace root: {e}"))?;
    if rel.is_empty() {
        return Ok(canon_root);
    }
    let candidate = canon_root.join(rel);
    let resolved = if candidate.exists() {
        candidate.canonicalize().map_err(|e| e.to_string())?
    } else {
        // A not-yet-existing file (e.g. the target of a fresh write) can't be
        // canonicalized itself — canonicalize its parent instead and reattach
        // the file name, which still catches `..` in `rel`.
        let parent = candidate.parent().ok_or("invalid path")?;
        let canon_parent = parent
            .canonicalize()
            .map_err(|_| "parent directory does not exist".to_string())?;
        canon_parent.join(candidate.file_name().ok_or("invalid path")?)
    };
    if !resolved.starts_with(&canon_root) {
        return Err("path escapes workspace root".to_string());
    }
    Ok(resolved)
}

/// Reduces a client-supplied id to something safe to use as a filename, a
/// SQLite key, and part of a URL path.
///
/// Prevents a caller from smuggling a path separator or `..` into an id that
/// later gets joined onto a data directory.
pub fn sanitize_id(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if cleaned.is_empty() {
        // Never return an empty id: it would collide with every other
        // degenerate input and, worse, silently widen scope.
        "default".to_string()
    } else {
        cleaned
    }
}

/// Derives a stable short id from a canonicalized workspace path.
///
/// FNV-1a over the path bytes: no new dependency, and the only properties that
/// matter here are determinism and a low collision rate on a handful of local
/// workspaces — not cryptographic strength. This value is a scoping key, never
/// a secret or an authentication token.
fn derive_id(path: &Path) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = FNV_OFFSET;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("ws_{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_id_is_sanitized() {
        let ws = WorkspaceId::explicit("my project", PathBuf::from("/tmp/a"));
        assert_eq!(ws.id(), "my_project");
        assert_eq!(ws.source(), WorkspaceSource::Explicit);
    }

    #[test]
    fn id_rejects_path_traversal_characters() {
        // A caller-supplied id must never be able to carry a separator or `..`
        // into a path it will later be joined onto.
        let ws = WorkspaceId::explicit("../../etc/passwd", PathBuf::from("/tmp/a"));
        assert!(!ws.id().contains('/'));
        assert!(!ws.id().contains(".."));
    }

    #[test]
    fn empty_id_falls_back_to_default() {
        // An empty id would collide with other degenerate input and silently
        // widen scope, so it gets a real name.
        let ws = WorkspaceId::explicit("", PathBuf::from("/tmp/a"));
        assert_eq!(ws.id(), "default");
    }

    #[test]
    fn overlong_id_is_bounded() {
        let ws = WorkspaceId::explicit("a".repeat(500), PathBuf::from("/tmp/a"));
        assert_eq!(ws.id().len(), 64);
    }

    #[test]
    fn derived_id_is_stable_across_calls() {
        let a = WorkspaceId::from_root(PathBuf::from("/tmp/workspace-a"));
        let b = WorkspaceId::from_root(PathBuf::from("/tmp/workspace-a"));
        assert_eq!(a.id(), b.id());
        assert_eq!(a.source(), WorkspaceSource::ConfiguredRoot);
    }

    #[test]
    fn different_roots_get_different_ids() {
        let a = WorkspaceId::from_root(PathBuf::from("/tmp/workspace-a"));
        let b = WorkspaceId::from_root(PathBuf::from("/tmp/workspace-b"));
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn missing_root_still_yields_an_id() {
        // Canonicalization fails for a not-yet-created workspace; a first chat
        // must not be rejected just because the directory isn't there yet.
        let ws = WorkspaceId::from_root(PathBuf::from("/definitely/not/here/at/all"));
        assert!(ws.id().starts_with("ws_"));
    }
}
