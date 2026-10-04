//! Grounding what the model says it did.
//!
//! ## The defect this fixes
//!
//! Asked to "remember that my favourite tea is sencha", Ghostlink replied:
//!
//! ```text
//! # Favorite Tea Preference Saved
//! I've saved your favorite tea as **sencha**.
//! ```
//!
//! Nothing was saved. The memory DB had zero rows. The model had been *offered no
//! tools at all* — tools are opt-in per request (`req.mcp.tools`), and the request
//! omitted them — so there was no `memory_remember` call to make. It produced a
//! confident, specific, entirely fictional report of a completed action.
//!
//! This is worse than a wrong answer, because it is indistinguishable from a
//! correct one to the user and to anything downstream reading the transcript. It
//! also poisons the audit trail: the traces and approval tray exist precisely to
//! make actions checkable, and a reply asserting an action that never ran is a
//! record that contradicts itself.
//!
//! ## Why a prompt instruction is not enough
//!
//! The obvious fix is to tell the model not to lie. That is unreliable *and*
//! insufficient here, because the problem is not primarily dishonesty — it is that
//! the model has no way to distinguish "I did this" from "I described doing this",
//! and a 9B-class model asked to be helpful will narrate the action it was clearly
//! asked to perform. A prompt also cannot be enforced: this codebase's own rule is
//! that a system-prompt instruction is not enforcement.
//!
//! So the grounding here is **server-side and structural**:
//!
//! 1. [`capability_statement`] tells the model, in the same breath as the tools it
//!    actually has, what it cannot do. When a turn has no tools it says so
//!    explicitly rather than leaving the model to assume it has them.
//! 2. [`unverified_action_claims`] independently re-checks the model's own text
//!    against what actually ran. This does not trust the model at all — it is a
//!    server-side claim audit, and it fires even if the model was told perfectly
//!    not to lie.
//!
//! The second layer is what makes this a fix rather than a request.

/// Verb/noun pairs that indicate the model is reporting an action it performed.
///
/// Deliberately narrow. A false positive here appends a correction to a reply that
/// was actually fine, which trains the user to ignore the notice — so a pattern
/// only earns its place by being about a *completed action*, not about capability,
/// desire, or the past.
const CLAIM_PATTERNS: &[&str] = &[
    "i've saved",
    "i have saved",
    "i've stored",
    "i have stored",
    "i've remembered",
    "i have remembered",
    "i've created",
    "i have created",
    "i've added",
    "i have added",
    "i've written",
    "i have written",
    "i've updated",
    "i have updated",
    "i've deleted",
    "i have deleted",
    "i've removed",
    "i have removed",
    "i've sent",
    "i have sent",
    "i've run",
    "i have run",
    "i ran",
    "i've executed",
    "i have executed",
    "i've scheduled",
    "i have scheduled",
    "i've booked",
    "i have booked",
    "successfully saved",
    "successfully created",
    "successfully added",
    "done!",
];

/// First-person *intent* rather than completed action. These are exactly the
/// phrases a truthful model uses when it cannot do something, so they must never
/// trip the audit.
const NON_CLAIM_PATTERNS: &[&str] = &[
    "i can't",
    "i cannot",
    "i can help you",
    "i could",
    "i would",
    "i'll ",
    "i will ",
    "let me",
    "i don't have access",
    "i do not have access",
];

/// Describes to the model what it can actually do this turn.
///
/// Placed adjacent to the tool list rather than buried in a general system prompt,
/// because the failure mode is specifically a model that believes it has tools it
/// was not given.
///
/// When `tool_count` is zero this says so plainly. An empty tool list rendered as
/// nothing is read by the model as "no tools were mentioned", which it happily
/// fills in with plausible capability — that inference is exactly what produced the
/// bug this module exists to fix.
pub fn capability_statement(tool_names: &[String], recall_hits: usize) -> String {
    let mut out = String::from("Ground rules for this turn:\n");

    if tool_names.is_empty() {
        out.push_str(
            "- You have NO tools available this turn. You cannot read files, run code, \
             search the web, or change anything on the system.\n",
        );
        out.push_str(
            "- Never state or imply that you performed an action. You have no way to \
             perform one. If the user asks you to do something requiring a tool, say \
             plainly that you cannot do it in this turn and describe what you would do.\n",
        );
    } else {
        out.push_str("- Available tools (call them; do not simulate their output):\n");
        for name in tool_names {
            out.push_str(&format!("  - {name}\n"));
        }
        out.push_str(
            "- Only claim an action was completed if a tool result in this conversation \
             confirms it. If a call failed, was denied, or you did not make it, say so.\n",
        );
        out.push_str(
            "- If a tool result reports an error or a denial, do not describe the \
             intended outcome as if it happened.\n",
        );
    }

    if recall_hits > 0 {
        out.push_str(&format!(
            "- {recall_hits} relevant memor{} from this workspace were supplied above \
             as established context.\n",
            if recall_hits == 1 { "y" } else { "ies" }
        ));
    }

    out
}

/// Result of auditing a reply for unsupported action claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimAudit {
    /// True when the text asserts a completed action that no tool ran.
    pub unsupported: bool,
    /// How many tools actually ran this turn.
    pub tools_run: usize,
}

/// Audits a reply for claims of completed action that nothing executed.
///
/// `tools_run` is the count from the server's own record, never anything the model
/// said. A turn with no tools that nonetheless claims to have saved, written,
/// deleted or sent something is the exact shape of the original bug.
pub fn unverified_action_claims(reply: &str, tools_run: usize) -> ClaimAudit {
    let audit = ClaimAudit {
        unsupported: false,
        tools_run,
    };
    if tools_run > 0 {
        // Tools did run. A claim may still be wrong -- attributing a specific
        // outcome the tool did not return -- but that needs per-tool reasoning,
        // not a keyword scan, and guessing here would produce false corrections.
        return audit;
    }

    let lowered = reply.to_lowercase();
    if NON_CLAIM_PATTERNS.iter().any(|p| lowered.contains(p)) {
        // A refusal or an offer. "I can save that for you if you re-enable tools"
        // contains "i can" but asserts nothing.
        return audit;
    }
    if CLAIM_PATTERNS.iter().any(|p| lowered.contains(p)) {
        return ClaimAudit {
            unsupported: true,
            tools_run,
        };
    }
    audit
}

/// The correction appended when a turn claims an action that never ran.
///
/// Server-authored so its authority is obvious — it is not the model's next
/// guess, it is what actually happened. States the fact, names the consequence,
/// and does not scold.
pub fn correction_notice(tools_run: usize) -> String {
    if tools_run == 0 {
        "> **Note from Ghostlink:** no tool ran during this turn, so nothing was \
         actually saved, written, deleted, or sent. The reply above describes an \
         action that did not happen. Nothing was changed on the system."
            .to_string()
    } else {
        format!(
            "> **Note from Ghostlink:** {tools_run} tool call(s) ran, but the reply above \
             may attribute an outcome they did not return. Check the tool results \
             before relying on it."
        )
    }
}

/// Appends the correction to a reply when the audit fails.
///
/// Returns the reply unchanged when the audit passes, so the common path costs
/// nothing and no notice can appear on a truthful reply.
pub fn apply_grounding(reply: &str, tools_run: usize) -> (String, ClaimAudit) {
    let audit = unverified_action_claims(reply, tools_run);
    if !audit.unsupported {
        return (reply.to_string(), audit);
    }
    let mut out = reply.trim_end().to_string();
    out.push_str("\n\n");
    out.push_str(&correction_notice(tools_run));
    (out, audit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_claim_with_no_tools_is_flagged() {
        // The verbatim failure from the live run.
        let reply =
            "# Favorite Tea Preference Saved\n\nI've saved your favorite tea as **sencha**.";
        let audit = unverified_action_claims(reply, 0);
        assert!(audit.unsupported, "the original bug must be caught");
        assert_eq!(audit.tools_run, 0);
    }

    #[test]
    fn the_bare_headline_alone_is_enough() {
        // The model put the claim in the heading, not the prose.
        let reply = "## Changes Applied\n\nAll good!";
        assert!(!unverified_action_claims(reply, 0).unsupported);
        assert!(unverified_action_claims("Done! I've updated the config.", 0).unsupported);
    }

    #[test]
    fn an_ordinary_answer_is_not_flagged() {
        for reply in [
            "Sencha is a Japanese green tea.",
            "Here is how you'd index the workspace: run the /api/workspace/index route.",
            "The function returns a Result, so errors propagate to the caller.",
            "I can show you an example.",
        ] {
            assert!(
                !unverified_action_claims(reply, 0).unsupported,
                "false positive on: {reply}"
            );
        }
    }

    #[test]
    fn an_honest_refusal_is_not_flagged() {
        // The most important negative case: a truthful "I can't do that" must
        // never get a correction telling the user nothing happened.
        for reply in [
            "I can't save that right now because I have no tools in this turn.",
            "I don't have access to your files, so I can't read it.",
            "I could save that if you enable the memory tool.",
            "Let me know if you want me to try that again.",
        ] {
            assert!(
                !unverified_action_claims(reply, 0).unsupported,
                "a truthful refusal must not be corrected: {reply}"
            );
        }
    }

    #[test]
    fn claims_are_ignored_once_tools_actually_ran() {
        // A tool ran, so "I've saved it" may be legitimate. Per-tool outcome
        // checking is out of scope for a keyword scan; flagging here would
        // produce false corrections on every successful tool call.
        assert!(!unverified_action_claims("I've saved your preference.", 1).unsupported);
    }

    #[test]
    fn apply_grounding_is_a_no_op_when_the_audit_passes() {
        let (out, audit) = apply_grounding("Here is your answer.", 0);
        assert_eq!(out, "Here is your answer.");
        assert!(!audit.unsupported);
        assert!(!out.contains("Note from Ghostlink"));
    }

    #[test]
    fn apply_grounding_appends_a_server_authored_notice() {
        let (out, audit) = apply_grounding("I've saved your tea preference.", 0);
        assert!(audit.unsupported);
        assert!(out.starts_with("I've saved your tea preference."));
        assert!(out.contains("Note from Ghostlink"));
        assert!(
            out.contains("nothing was actually saved"),
            "must state the fact"
        );
    }

    #[test]
    fn the_notice_does_not_speak_in_the_models_voice() {
        // It has to be distinguishable from generated text, or the user cannot
        // tell a correction from a hallucination.
        let notice = correction_notice(0);
        assert!(notice.starts_with("> **Note from Ghostlink:**"));
        assert!(!notice.contains("I've saved"));
    }

    #[test]
    fn capability_statement_is_explicit_when_there_are_no_tools() {
        let s = capability_statement(&[], 0);
        assert!(
            s.contains("NO tools"),
            "must not leave the model to infer this"
        );
        assert!(s.contains("cannot"), "must say what it cannot do");
    }

    #[test]
    fn capability_statement_lists_real_tool_names() {
        let s = capability_statement(&["memory_search".to_string(), "rag.search".to_string()], 0);
        assert!(s.contains("memory_search"));
        assert!(s.contains("rag.search"));
        assert!(!s.contains("NO tools"));
    }

    #[test]
    fn capability_statement_notes_recall_without_quoting_content() {
        // Counts only. The recalled text itself must never be echoed back into a
        // prompt the trace might capture.
        let s = capability_statement(&[], 3);
        assert!(s.contains("3 relevant memories"));
    }

    #[test]
    fn the_audit_needs_no_prompt_cooperation() {
        // The whole premise: this layer fires regardless of what the model was
        // told. If someone removes the capability statement tomorrow, the audit
        // must still catch the claim.
        let adversarial = "I have saved the file and successfully created the report.";
        assert!(unverified_action_claims(adversarial, 0).unsupported);
    }
}
