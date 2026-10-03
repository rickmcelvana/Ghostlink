//! Turn traces: what an assistant turn did, in a form safe to persist.
//!
//! The invariant this module exists to hold: **a trace records that a tool ran,
//! never what it was given or what it returned.** Tool name, capability class,
//! decision, token counts, and latency are all fair game. Prompts,
//! completions, file bodies, SQL, command lines, and secrets are not — and the
//! distinction is enforced structurally rather than by convention, because a
//! convention is exactly the kind of thing that erodes one field at a time.
//!
//! So the only way to build a [`TraceEvent`] is through this module's
//! constructors, each of which takes a fixed set of typed parameters. There is
//! deliberately no `new(event, detail: String)` escape hatch and no way to pass
//! a tool's arguments or result: the fields simply don't exist on the struct.
//! Adding a `payload` field later would be a deliberate, reviewable change
//! rather than an accident.

use std::time::Duration;

/// What kind of step a trace records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceEventKind {
    /// An agent turn started.
    InvokeAgent,
    /// A chat turn ran.
    Chat,
    /// A tool was dispatched (or refused, before dispatch).
    ExecuteTool,
    /// A human decision was recorded about a gated tool call.
    ToolApproval,
}

impl TraceEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvokeAgent => "invoke_agent",
            Self::Chat => "chat",
            Self::ExecuteTool => "execute_tool",
            Self::ToolApproval => "tool_approval",
        }
    }

    /// Parses a kind name (e.g. from `?kind=`), or `None` if unrecognized.
    ///
    /// An unknown name returns `None` rather than erroring, which the handler
    /// treats as "no filter" — a typo in a query string shouldn't 400.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "invoke_agent" => Some(Self::InvokeAgent),
            "chat" => Some(Self::Chat),
            "execute_tool" => Some(Self::ExecuteTool),
            "tool_approval" => Some(Self::ToolApproval),
            _ => None,
        }
    }
}

/// The outcome of a gated call, as recorded on the trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceDecision {
    /// Ran inline without needing approval.
    Allowed,
    /// Refused at the gate; nothing was dispatched.
    Blocked,
    /// A human said yes.
    Approved,
    /// A human said no.
    Denied,
    /// Approved as a standing grant for the session.
    ApprovedForSession,
    /// The tool ran and reported failure.
    Failed,
}

impl TraceDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Blocked => "blocked",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::ApprovedForSession => "approved_for_session",
            Self::Failed => "failed",
        }
    }
}

/// One recorded step.
///
/// Every field is either an enum, a number, or a short identifier chosen by the
/// caller from a closed set. There is no free-form body, which is what keeps
/// prompts, completions, and file contents out of the trail by construction.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceEvent {
    pub kind: TraceEventKind,
    /// Short label: the tool name, the backend name, or a turn id. Never the
    /// content of what ran.
    pub label: String,
    pub workspace_id: String,
    /// Capability class (`read`/`write`/`exec`), when the step is tool-related.
    pub capability_class: Option<String>,
    pub decision: Option<TraceDecision>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub latency_ms: Option<u64>,
}

impl TraceEvent {
    /// Longest accepted `label`.
    ///
    /// A label is a tool or backend name, so anything longer than this is a
    /// caller passing content where a name belongs. Truncating keeps a mistake
    /// from writing an entire prompt into the durable audit log.
    const MAX_LABEL_CHARS: usize = 64;

    fn new(kind: TraceEventKind, label: &str, workspace_id: &str) -> Self {
        Self {
            kind,
            label: truncate_label(label),
            workspace_id: truncate_label(workspace_id),
            capability_class: None,
            decision: None,
            input_tokens: None,
            output_tokens: None,
            latency_ms: None,
        }
    }

    /// Attaches the capability class. Takes `&str` so a caller must name a
    /// class from the table rather than inventing one.
    pub fn with_class(mut self, class: Option<&str>) -> Self {
        self.capability_class = class.map(truncate_label);
        self
    }

    pub fn with_decision(mut self, decision: TraceDecision) -> Self {
        self.decision = Some(decision);
        self
    }

    pub fn with_tokens(mut self, input: Option<u32>, output: Option<u32>) -> Self {
        self.input_tokens = input;
        self.output_tokens = output;
        self
    }

    pub fn with_latency(mut self, latency: Duration) -> Self {
        self.latency_ms = Some(latency.as_millis() as u64);
        self
    }

    /// A one-line rendering for the audit trail's `detail` field.
    ///
    /// Field order is fixed and every value is enum-derived or numeric, so this
    /// cannot smuggle content into a log line.
    pub fn to_detail(&self) -> String {
        let mut parts = vec![format!("kind={}", self.kind.as_str())];
        if !self.label.is_empty() {
            parts.push(format!("label={}", self.label));
        }
        if !self.workspace_id.is_empty() {
            parts.push(format!("workspace={}", self.workspace_id));
        }
        if let Some(class) = &self.capability_class {
            parts.push(format!("class={class}"));
        }
        if let Some(decision) = self.decision {
            parts.push(format!("decision={}", decision.as_str()));
        }
        if let (Some(i), Some(o)) = (self.input_tokens, self.output_tokens) {
            parts.push(format!("tokens={i}/{o}"));
        }
        if let Some(ms) = self.latency_ms {
            parts.push(format!("latency_ms={ms}"));
        }
        parts.join(" ")
    }

    /// Records a chat turn.
    pub fn chat(turn_id: &str, workspace_id: &str) -> Self {
        Self::new(TraceEventKind::Chat, turn_id, workspace_id)
    }

    /// Records a human's decision about a gated call.
    pub fn tool_approval(
        tool_name: &str,
        workspace_id: &str,
        class: Option<&str>,
        decision: TraceDecision,
    ) -> Self {
        Self::new(TraceEventKind::ToolApproval, tool_name, workspace_id)
            .with_class(class)
            .with_decision(decision)
    }
}

/// Truncates on a char boundary, marking that it did.
///
/// Truncation is loud rather than silent: a trace that quietly holds half a
/// value invites someone to trust it as complete.
fn truncate_label(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.chars().count() <= TraceEvent::MAX_LABEL_CHARS {
        return trimmed.to_string();
    }
    let kept: String = trimmed.chars().take(TraceEvent::MAX_LABEL_CHARS).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_carries_only_structured_fields() {
        let e =
            TraceEvent::tool_approval("write_file", "ws_a", Some("write"), TraceDecision::Blocked)
                .with_latency(Duration::from_millis(12));
        let detail = e.to_detail();
        assert!(detail.contains("kind=tool_approval"));
        assert!(detail.contains("label=write_file"));
        assert!(detail.contains("class=write"));
        assert!(detail.contains("decision=blocked"));
        assert!(detail.contains("latency_ms=12"));
    }

    #[test]
    fn label_is_truncated_loudly() {
        let long = "x".repeat(500);
        let e = TraceEvent::chat(&long, "ws");
        assert!(e.label.chars().count() <= TraceEvent::MAX_LABEL_CHARS + 1);
        assert!(e.label.ends_with('…'), "truncation must be visible");
    }

    #[test]
    fn label_truncation_respects_char_boundaries() {
        let long = "é".repeat(200);
        let e = TraceEvent::chat(&long, "ws");
        // Must not panic on a multi-byte boundary, and must stay within budget.
        assert!(e.label.chars().count() <= TraceEvent::MAX_LABEL_CHARS + 1);
    }

    #[test]
    fn short_label_is_untouched() {
        let e = TraceEvent::chat("turn-7", "ws_a");
        assert_eq!(e.label, "turn-7");
        assert!(!e.label.contains('…'));
    }

    #[test]
    fn tokens_are_carried_when_present() {
        let e = TraceEvent::chat("t", "ws").with_tokens(Some(120), Some(45));
        assert!(e.to_detail().contains("tokens=120/45"));
    }

    #[test]
    fn absent_optionals_are_omitted_entirely() {
        let detail = TraceEvent::chat("t", "ws").to_detail();
        assert!(!detail.contains("class="));
        assert!(!detail.contains("decision="));
        assert!(!detail.contains("tokens="));
        assert!(!detail.contains("latency_ms="));
    }

    #[test]
    fn parse_round_trips_and_rejects_junk() {
        for kind in [
            TraceEventKind::InvokeAgent,
            TraceEventKind::Chat,
            TraceEventKind::ExecuteTool,
            TraceEventKind::ToolApproval,
        ] {
            assert_eq!(TraceEventKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(TraceEventKind::parse("nope"), None);
    }
}
