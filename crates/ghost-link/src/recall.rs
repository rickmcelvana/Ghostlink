//! Proactive recall: pull relevant memory and indexed documents into a turn.
//!
//! ## Why this exists
//!
//! Ghostlink can *store* memory (`mcp-memory`) and *index* documents (`mcp-rag`),
//! and the model can call `memory_search` / `rag.search` itself. But nothing ever
//! invoked them. Every reference to those tools in the server lived in
//! `capability.rs`, classifying them -- none of them called them.
//!
//! The result was a filing cabinet nobody opened: a user asking "what did we
//! decide about the auth flow?" got a blank look unless they happened to know to
//! ask the model to search its own memory. Retrieval that depends on the user
//! knowing to request it is not memory, it's a database with extra steps.
//!
//! ## What this does
//!
//! On the **first turn of a session**, run both retrievers server-side and inject
//! the hits as a system message ahead of the conversation. The model then knows
//! what it already knows without having to spend a tool call discovering that it
//! has a memory.
//!
//! ## Design constraints
//!
//! - **Bounded.** Every hit is truncated and the whole block is token-capped.
//!   Injected context is still context the model has to pay for on every
//!   subsequent turn, so an unbounded recall would be a slow leak.
//! - **Best-effort, never fatal.** A missing, disconnected or erroring retriever
//!   yields no recall block and the turn proceeds. Retrieval is an enhancement;
//!   it must never be able to fail a request.
//! - **Read-only.** This calls `memory_search` and `rag.search` only -- the two
//!   `CapabilityClass::Read` tools. It never writes, so it needs no approval and
//!   cannot be prompted into recording something.
//! - **Workspace-scoped.** The scope is stamped server-side from the request's
//!   `workspace_id`, exactly as a model-issued call would be, so recall cannot
//!   leak another workspace's memories.
//! - **Never logs content.** Only counts reach the caller and the trace, matching
//!   the rule that traces carry no prompt or completion text.

use serde_json::{json, Value};

/// Characters of one retrieved item kept. Long enough for a memory title plus a
/// useful sentence, short enough that a runaway document chunk cannot dominate.
const MAX_ITEM_CHARS: usize = 600;

/// Total characters of the assembled recall block. Roughly a few hundred tokens --
/// enough to be useful, small enough to be nearly free.
const MAX_TOTAL_CHARS: usize = 2_400;

/// How many hits to ask each retriever for.
pub const TOP_K: usize = 5;

/// What was recalled, for observability. Deliberately counts only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecallOutcome {
    /// Rows returned by `memory_search`.
    pub memory_hits: usize,
    /// Hits returned by `rag.search`.
    pub rag_hits: usize,
    /// Characters of the assembled block (0 when nothing was recalled).
    pub block_chars: usize,
    /// Whether either retriever was unreachable, so a caller can tell
    /// "nothing remembered" from "nothing could be asked".
    pub retriever_failed: bool,
}

impl RecallOutcome {
    pub fn any(&self) -> bool {
        self.block_chars > 0
    }
}

/// One retrieved item, reduced to the minimum the prompt needs.
pub struct RecallItem {
    pub source: &'static str,
    pub title: String,
    pub body: String,
}

fn truncate_chars(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max).collect();
    format!("{head}...")
}

/// Pulls `title`/`text`/`body`/`content` out of a memory row or a RAG hit.
///
/// The two servers do not agree on a field name -- `mcp-memory` rows carry
/// `title`/`body`, RAG hits carry `source`/`text` -- so this accepts either shape
/// rather than hard-coding one and silently dropping the other's results.
fn item_from_value(source: &'static str, v: &Value) -> Option<RecallItem> {
    let obj = v.as_object()?;
    let pick = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|k| obj.get(*k).and_then(|x| x.as_str()))
            .unwrap_or("")
            .to_string()
    };
    let title = pick(&["title", "source", "name"]);
    let body = pick(&["body", "text", "content", "snippet"]);
    if title.is_empty() && body.is_empty() {
        return None;
    }
    Some(RecallItem {
        source,
        title: truncate_chars(&title, 120),
        body: truncate_chars(&body, MAX_ITEM_CHARS),
    })
}

/// Builds the system message. Returns `None` when there is nothing to say, so the
/// caller adds no message at all rather than an empty one.
fn assemble(items: &[RecallItem]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut out = String::from(
        "Relevant context recalled from your memory and indexed documents. \
         Treat it as established fact about this user and project unless the \
         conversation contradicts it.\n",
    );
    for item in items {
        let mut line = format!("- [{}]", item.source);
        if !item.title.is_empty() {
            line.push_str(&format!(" {}:", item.title));
        }
        line.push(' ');
        line.push_str(&item.body);
        out.push_str(&line);
        out.push('\n');
        if out.chars().count() >= MAX_TOTAL_CHARS {
            break;
        }
    }
    let capped: String = out.chars().take(MAX_TOTAL_CHARS).collect();
    Some(capped.trim_end().to_string())
}

/// Arguments for `memory_search`. Kept here so the shape is unit-testable without
/// an MCP server.
pub fn memory_search_args(query: &str, workspace_id: &str, limit: usize) -> Value {
    json!({
        "query": query,
        "limit": limit,
        // Stamped server-side; never taken from the model.
        "workspace_id": workspace_id,
    })
}

/// Arguments for `rag.search`.
pub fn rag_search_args(query: &str, top_k: usize) -> Value {
    json!({ "query": query, "top_k": top_k })
}

/// Extracts recall items from a tool result.
///
/// MCP tool results arrive as `content: [{type: "text", text: "<json>"}]`, and a
/// server may also return the payload directly. Both are handled; anything
/// unrecognised yields nothing rather than guessing.
pub fn items_from_outcome(source: &'static str, outcome: &Value) -> Vec<RecallItem> {
    // Preferred shape: content[].text holding JSON.
    if let Some(content) = outcome.get("content").and_then(|c| c.as_array()) {
        let mut items = Vec::new();
        for block in content {
            let Some(text) = block.get("text").and_then(|t| t.as_str()) else {
                continue;
            };
            let parsed: Value = match serde_json::from_str(text) {
                Ok(p) => p,
                Err(_) => continue,
            };
            items.extend(collect_rows(&parsed, source));
        }
        if !items.is_empty() {
            return items;
        }
    }
    collect_rows(outcome, source)
}

/// Finds the row array inside a payload of either shape: a bare array, or an
/// object under one of the usual keys.
fn collect_rows(v: &Value, source: &'static str) -> Vec<RecallItem> {
    let rows: Vec<&Value> = if let Some(arr) = v.as_array() {
        arr.iter().collect()
    } else {
        ["results", "entries", "memories", "hits", "documents"]
            .iter()
            .find_map(|k| v.get(*k).and_then(|x| x.as_array()))
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    };
    rows.into_iter()
        .filter_map(|r| item_from_value(source, r))
        .take(TOP_K)
        .collect()
}

/// Renders the recall block for a turn, given both raw tool outcomes.
///
/// Pure: takes the outcomes, returns the message and the counts. The MCP calls
/// themselves happen in `recall_for_turn` so this stays testable.
pub fn build_recall_block(
    memory_outcome: Option<&Value>,
    rag_outcome: Option<&Value>,
) -> (Option<String>, RecallOutcome) {
    let mut items = Vec::new();
    let mut outcome = RecallOutcome::default();

    if let Some(v) = memory_outcome {
        items.extend(items_from_outcome("memory", v));
    }
    if let Some(v) = rag_outcome {
        items.extend(items_from_outcome("rag", v));
    }
    outcome.memory_hits = items.iter().filter(|i| i.source == "memory").count();
    outcome.rag_hits = items.iter().filter(|i| i.source == "rag").count();

    let block = assemble(&items);
    outcome.block_chars = block.as_ref().map(|b| b.chars().count()).unwrap_or(0);
    (block, outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The implementation source excluding this module. Read via `include_str!` so
    /// the guard below can only ever scan this file, the one it was written in.
    fn impl_source() -> &'static str {
        include_str!("recall.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap_or_default()
    }

    #[test]
    fn no_outcomes_means_no_block() {
        let (block, outcome) = build_recall_block(None, None);
        assert!(
            block.is_none(),
            "no retrievers configured must add no message"
        );
        assert!(!outcome.any());
        assert_eq!(outcome.memory_hits, 0);
        assert_eq!(outcome.rag_hits, 0);
    }

    #[test]
    fn empty_result_sets_produce_no_block() {
        let (block, outcome) = build_recall_block(Some(&json!([])), Some(&json!([])));
        assert!(block.is_none(), "an empty hit list is not worth a message");
        assert!(!outcome.any());
    }

    #[test]
    fn reads_the_mcp_content_text_envelope() {
        // The shape mcp-memory actually returns: JSON inside content[].text.
        let outcome = json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&json!([{
                    "title": "auth decision",
                    "body": "we chose short-lived JWTs over a raw key endpoint"
                }])).unwrap()
            }]
        });
        let (block, outcome) = build_recall_block(Some(&outcome), None);
        let b = block.expect("a hit should produce a block");
        assert!(b.contains("auth decision"));
        assert!(b.contains("short-lived JWTs"));
        assert_eq!(outcome.memory_hits, 1);
    }

    #[test]
    fn reads_the_rag_hit_shape() {
        // RAG hits carry `source`/`text`, not `title`/`body`.
        let outcome = json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&json!([{
                    "source": "docs/SECURITY_MODEL.md",
                    "text": "tool classification is enforced server-side",
                    "score": 0.71
                }])).unwrap()
            }]
        });
        let (block, outcome) = build_recall_block(None, Some(&outcome));
        let b = block.expect("a hit should produce a block");
        assert!(b.contains("SECURITY_MODEL.md"));
        assert!(b.contains("enforced server-side"));
        assert_eq!(outcome.rag_hits, 1);
    }

    #[test]
    fn reads_a_bare_array_with_no_envelope() {
        let (block, _) = build_recall_block(Some(&json!([{"title": "t", "body": "b"}])), None);
        assert!(block.is_some());
    }

    #[test]
    fn reads_a_wrapped_object() {
        let outcome = json!({ "results": [{ "title": "wrapped", "body": "still found" }] });
        let (block, _) = build_recall_block(Some(&outcome), None);
        assert!(block.expect("wrapped shape").contains("still found"));
    }

    #[test]
    fn garbage_yields_nothing_rather_than_garbage() {
        let outcome = json!({ "content": [{ "type": "text", "text": "not json at all" }] });
        let (block, _) = build_recall_block(Some(&outcome), None);
        assert!(block.is_none(), "unparseable text must not become context");
    }

    #[test]
    fn a_block_is_token_capped() {
        let huge: Vec<Value> = (0..TOP_K)
            .map(|i| json!({ "title": format!("item {i}"), "body": "x".repeat(50_000) }))
            .collect();
        let outcome = json!({
            "content": [{ "type": "text", "text": serde_json::to_string(&huge).unwrap() }]
        });
        let (block, outcome) = build_recall_block(Some(&outcome), None);
        let b = block.expect("hits should still produce a capped block");
        assert!(
            b.chars().count() <= MAX_TOTAL_CHARS,
            "block was {} chars, cap is {MAX_TOTAL_CHARS}",
            b.chars().count()
        );
        assert!(outcome.memory_hits > 0, "but hits are still counted");
    }

    #[test]
    fn per_item_text_is_truncated() {
        let outcome = json!([{ "title": "t", "body": "y".repeat(MAX_ITEM_CHARS * 3) }]);
        let (block, _) = build_recall_block(Some(&outcome), None);
        let b = block.expect("one hit");
        assert!(
            b.chars().count() < MAX_ITEM_CHARS * 2,
            "a single item was not trimmed: {} chars",
            b.chars().count()
        );
        assert!(b.contains("..."), "truncation should be visible");
    }

    #[test]
    fn top_k_bounds_hits_per_retriever() {
        let many: Vec<Value> = (0..50)
            .map(|i| json!({ "title": format!("item {i}"), "body": "b" }))
            .collect();
        let outcome = json!({ "content": [{
            "type": "text", "text": serde_json::to_string(&many).unwrap()
        }] });
        let (_, outcome) = build_recall_block(Some(&outcome), None);
        assert!(
            outcome.memory_hits <= TOP_K,
            "took {} hits, TOP_K is {TOP_K}",
            outcome.memory_hits
        );
    }

    #[test]
    fn workspace_scope_is_stamped_from_the_server_never_the_model() {
        // The stamp only ever arrives as a function argument, so there is no path
        // by which a model-selected workspace id could reach the query.
        let args = memory_search_args("what did we decide", "ws_abc", TOP_K);
        assert_eq!(args["workspace_id"], "ws_abc");
        assert_eq!(args["limit"], TOP_K);
        assert_eq!(args["query"], "what did we decide");
    }

    #[test]
    fn rag_args_carry_the_query_and_k() {
        let args = rag_search_args("auth flow", TOP_K);
        assert_eq!(args["query"], "auth flow");
        assert_eq!(args["top_k"], TOP_K);
    }

    #[test]
    fn recall_never_names_a_write_tool_in_its_call_path() {
        // Only the two read tools are ever referenced. A write tool in the call
        // path would let recall persist something without going through approval.
        //
        // Scans only the implementation, because this test necessarily contains
        // the forbidden names as data.
        let implementation = impl_source();
        for name in ["memory_remember", "memory_forget", "rag.index"] {
            assert!(
                !implementation.contains(name),
                "recall must never reference a write tool in its call path: {name}"
            );
        }
    }
}
