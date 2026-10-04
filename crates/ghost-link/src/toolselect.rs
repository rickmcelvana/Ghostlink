//! Which tool slots a turn runs with, when the client did not say.
//!
//! ## The problem
//!
//! `req.mcp.tools` was the only source of enabled tool slots, defaulting to empty:
//!
//! ```text
//! .and_then(|t| t.as_array())
//! .map(...)
//! .unwrap_or_default();   // -> no tools at all
//! ```
//!
//! So any client that omitted `mcp.tools` got an assistant with **no tools** —
//! silently, and with nothing in the response to say so. Observed live: a request
//! asked to store a memory, the model had nothing to call, and it narrated a
//! completed save over an empty database. `grounding` now catches that claim, but
//! catching the lie is not the same as giving the model the ability to act.
//!
//! Scheduled turns and any non-GUI client hit this permanently. The GUI happens to
//! send its checkbox list, which is why it was never obvious.
//!
//! ## The rule
//!
//! | request | meaning |
//! |---|---|
//! | `mcp.tools` absent | server default: every connected server's **read-only** tools |
//! | `mcp.tools: []` | explicitly none — honored as sent |
//! | `mcp.tools: [...]` | exactly those slots |
//!
//! The absent/empty distinction matters. A client that sends an empty array has
//! decided it wants no tools, and silently overriding that would be the same class
//! of bug in the opposite direction — the user's explicit "none" becoming "all the
//! reads".
//!
//! ## Why reads only
//!
//! The default cannot be "everything enabled". `CapabilityClass` is the trust
//! boundary, and `Write` and `Exec` are gated per call for good reasons. Defaulting
//! them on would mean a client that forgot a field got a shell — and it would
//! break the project's own rule that unlisted capability fails closed. So the
//! default is exactly the set that `classify` calls `Read`, which by construction
//! needs no approval and cannot change anything.

use crate::capability::CapabilityClass;

/// What the client asked for, if anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSelection {
    /// The field was absent — use [`default_read_slots`].
    Unspecified,
    /// The client sent an explicit list, possibly empty. Honor it verbatim.
    Explicit(Vec<String>),
}

/// Reads the selection out of a request's `mcp` object.
///
/// `None` for the whole `mcp` field and for a missing `tools` key both mean
/// unspecified. A present-but-empty array is `Explicit(vec![])` and must stay
/// that way.
pub fn selection_from_request(mcp: Option<&serde_json::Value>) -> ToolSelection {
    let tools = mcp.and_then(|v| v.get("tools"));
    match tools {
        None => ToolSelection::Unspecified,
        Some(serde_json::Value::Array(items)) => ToolSelection::Explicit(
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        ),
        // A non-array `tools` is a malformed request, not an instruction. Treat it
        // as unspecified rather than as "no tools", so a client bug cannot
        // silently disarm the assistant.
        Some(_) => ToolSelection::Unspecified,
    }
}

/// Default slots: every slot that has at least one `Read` tool.
///
/// A slot qualifies when it offers *any* read. Write and Exec tools on that server
/// remain reachable -- and remain gated by `capability::decide` at dispatch; they
/// are simply not what makes a slot eligible for the default.
///
/// The alternative, excluding any slot containing a Write, was tried first and is
/// wrong in practice: it excluded `memory` and `rag`, whose servers each mix
/// `memory_search` with `memory_remember` and `search` with `index_document`. The
/// default then offered exactly one slot (`sequential-thinking`) while four servers
/// were connected -- "use the default" quietly meaning "you get almost nothing",
/// which is the same silent-disablement this module exists to remove.
///
/// Exposing a Write tool is not granting it. The capability gate is evaluated per
/// call, so offering `memory_remember` by default still routes it through the
/// vetted-auto-apply or approval path exactly as before. This changes *visibility*,
/// never *permission*.
///
/// An Exec-only slot does not qualify: it has nothing readable to contribute to a
/// default whose purpose is letting the assistant answer.
pub fn default_read_slots(slots: &[(String, Vec<(String, CapabilityClass)>)]) -> Vec<String> {
    slots
        .iter()
        .filter(|(_, tools)| {
            tools
                .iter()
                .any(|(_, class)| *class == CapabilityClass::Read)
                && !tools
                    .iter()
                    .all(|(_, class)| *class == CapabilityClass::Exec)
        })
        .map(|(slot, _)| slot.clone())
        .collect()
}

/// Resolves the slots a turn should run with.
///
/// `connected` is `(slot, [(tool, class)])` for the servers actually reachable
/// this run — an unreachable server contributes nothing, since offering a tool the
/// model cannot call is worse than not offering it.
pub fn resolve_slots(
    selection: &ToolSelection,
    connected: &[(String, Vec<(String, CapabilityClass)>)],
) -> Vec<String> {
    match selection {
        ToolSelection::Explicit(slots) => slots.clone(),
        ToolSelection::Unspecified => default_read_slots(connected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read(name: &str) -> (String, CapabilityClass) {
        (name.to_string(), CapabilityClass::Read)
    }
    fn write(name: &str) -> (String, CapabilityClass) {
        (name.to_string(), CapabilityClass::Write)
    }
    fn exec(name: &str) -> (String, CapabilityClass) {
        (name.to_string(), CapabilityClass::Exec)
    }

    #[test]
    fn an_absent_tools_field_means_unspecified() {
        assert_eq!(selection_from_request(None), ToolSelection::Unspecified);
        assert_eq!(
            selection_from_request(Some(&json!({}))),
            ToolSelection::Unspecified
        );
        assert_eq!(
            selection_from_request(Some(&json!({"other": 1}))),
            ToolSelection::Unspecified
        );
    }

    #[test]
    fn an_explicit_empty_list_is_honored_as_none() {
        // The asymmetry that matters: `[]` is a decision, not an omission. Honoring
        // the server default here would override a client that deliberately asked
        // for no tools.
        let sel = selection_from_request(Some(&json!({ "tools": [] })));
        assert_eq!(sel, ToolSelection::Explicit(vec![]));
        let connected = vec![("memory".to_string(), vec![read("memory_search")])];
        assert!(resolve_slots(&sel, &connected).is_empty());
    }

    #[test]
    fn an_explicit_list_is_passed_through_verbatim() {
        let sel = selection_from_request(Some(&json!({ "tools": ["rag", "memory"] })));
        assert_eq!(
            resolve_slots(&sel, &[]),
            vec!["rag".to_string(), "memory".to_string()]
        );
    }

    #[test]
    fn a_malformed_tools_value_does_not_silently_disarm_the_assistant() {
        // A client bug sending `"tools": "memory"` should not cost the user their
        // tools. Treated as unspecified so the read-only default applies.
        let sel = selection_from_request(Some(&json!({ "tools": "memory" })));
        assert_eq!(sel, ToolSelection::Unspecified);
    }

    #[test]
    fn unspecified_defaults_to_read_only_slots() {
        let connected = vec![
            (
                "memory".to_string(),
                vec![read("memory_search"), read("memory_catalog")],
            ),
            ("rag".to_string(), vec![read("search")]),
        ];
        let slots = resolve_slots(&ToolSelection::Unspecified, &connected);
        assert!(slots.contains(&"memory".to_string()));
        assert!(slots.contains(&"rag".to_string()));
    }

    #[test]
    fn a_slot_with_no_tools_is_not_offered() {
        // Offering an empty list advertises capability that cannot be exercised.
        let connected = vec![("memory".to_string(), vec![])];
        assert!(default_read_slots(&connected).is_empty());
    }

    #[test]
    fn a_disconnected_server_contributes_nothing() {
        let connected = vec![("memory".to_string(), vec![read("memory_search")])];
        let slots = resolve_slots(&ToolSelection::Unspecified, &connected);
        assert_eq!(slots, vec!["memory".to_string()]);
        // And with nothing connected at all, the model is correctly toolless --
        // and `grounding` will then say so rather than let it improvise.
        assert!(resolve_slots(&ToolSelection::Unspecified, &[]).is_empty());
    }

    #[test]
    fn a_slot_qualifies_when_it_has_any_read() {
        // `memory` and `rag` each mix reads with writes. Excluding such slots
        // wholesale meant the default offered only `sequential-thinking` while four
        // servers were connected -- the silent-disablement this module fixes. This
        // test is the regression guard for exactly that.
        let connected = vec![
            (
                "memory".to_string(),
                vec![read("memory_search"), write("memory_remember")],
            ),
            (
                "rag".to_string(),
                vec![read("search"), write("index_document")],
            ),
            (
                "sequential-thinking".to_string(),
                vec![read("sequentialthinking")],
            ),
        ];
        let slots = default_read_slots(&connected);
        for expected in ["memory", "rag", "sequential-thinking"] {
            assert!(
                slots.iter().any(|s| s == expected),
                "{expected} should be in the default, got {slots:?}"
            );
        }
    }

    #[test]
    fn a_write_becomes_visible_but_stays_gated() {
        // The security-relevant half of the previous decision: offering a Write
        // tool is not granting it. `decide` still runs per call, so the default
        // changes visibility, never permission.
        let connected = vec![(
            "memory".to_string(),
            vec![read("memory_search"), write("memory_remember")],
        )];
        assert!(default_read_slots(&connected).contains(&"memory".to_string()));

        let forget = crate::capability::decide(
            "ws",
            "memory",
            "memory_forget",
            None,
            std::path::Path::new("."),
        );
        assert!(
            matches!(forget, crate::capability::Decision::NeedsApproval { .. }),
            "memory_forget must still require approval, got {forget:?}"
        );
        let gateway = crate::capability::decide(
            "ws",
            "docker-mcp-gateway",
            "mcp-exec",
            None,
            std::path::Path::new("."),
        );
        assert!(
            matches!(gateway, crate::capability::Decision::NeedsApproval { .. }),
            "docker exec must still require approval, got {gateway:?}"
        );
    }

    #[test]
    fn an_exec_only_slot_is_never_offered_by_default() {
        // Nothing readable in it, so nothing to contribute to a default whose
        // purpose is letting the assistant answer.
        let connected = vec![("shell".to_string(), vec![exec("run_command")])];
        assert!(default_read_slots(&connected).is_empty());
    }

    #[test]
    fn the_default_matches_the_real_classifier_for_shipped_servers() {
        // Guards against the table drifting away from the classifier: whatever a
        // server advertises, the decision is made from `classify`, not from a
        // hand-maintained list here.
        let connected = vec![
            (
                "docker-mcp-gateway".to_string(),
                vec![
                    (
                        "mcp-exec".to_string(),
                        crate::capability::classify("docker-mcp-gateway", "mcp-exec"),
                    ),
                    (
                        "code-mode".to_string(),
                        crate::capability::classify("docker-mcp-gateway", "code-mode"),
                    ),
                ],
            ),
            (
                "rag".to_string(),
                vec![
                    (
                        "search".to_string(),
                        crate::capability::classify("rag", "search"),
                    ),
                    (
                        "index_document".to_string(),
                        crate::capability::classify("rag", "index_document"),
                    ),
                ],
            ),
            (
                "memory".to_string(),
                vec![
                    (
                        "memory_search".to_string(),
                        crate::capability::classify("memory", "memory_search"),
                    ),
                    (
                        "memory_remember".to_string(),
                        crate::capability::classify("memory", "memory_remember"),
                    ),
                ],
            ),
        ];
        let slots = default_read_slots(&connected);
        assert!(
            slots.contains(&"rag".to_string()),
            "rag has a read: {slots:?}"
        );
        assert!(
            slots.contains(&"memory".to_string()),
            "memory has a read: {slots:?}"
        );
        assert!(
            !slots.contains(&"docker-mcp-gateway".to_string()),
            "all-Exec gateway must stay out of the default: {slots:?}"
        );
    }
}
