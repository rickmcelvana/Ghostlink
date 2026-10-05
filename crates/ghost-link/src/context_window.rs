//! Bounding what a chat turn resends.
//!
//! ## The problem
//!
//! `trim_conversation_history` walks backwards from the newest turn and keeps
//! whatever fits `conversation_token_limit`. That is a token ceiling and nothing
//! more, which leaves two holes on the hot path:
//!
//! 1. **No completion reserve.** The budget covers history only. A turn that fills
//!    it exactly leaves the model no room to answer, and the reply gets truncated
//!    against the model's own context rather than against a limit chosen here.
//!
//! 2. **No keep-last-turns policy.** The memo's `GHOSTLINK_KEEP_LAST_TURNS` does not
//!    exist anywhere in the codebase. Every turn re-evaluates the whole history
//!    against the budget and can resend all of it. Measured on this machine, a
//!    5,300-token prompt prefill took 21.7s -- so a long conversation pays that on
//!    *every* turn, and TTFT grows without bound until the ceiling finally bites.
//!
//! ## What this adds
//!
//! A sliding window with two independent controls:
//!
//! - a **completion reserve**, so history is budgeted against
//!   `context - reserve - reply` rather than the full context;
//! - a **keep-last-turns floor**, so the most recent exchange survives even when it
//!   alone exceeds the budget -- dropping the newest turns is the one outcome that
//!   makes a conversation incoherent, since the model loses what was just said.
//!
//! ## Why the floor exists
//!
//! The existing trim `break`s when a turn does not fit, so it always keeps the
//! newest. That is right, and this keeps it: the floor is a *lower* bound on
//! recency, not a licence to keep everything. A policy that preserved old turns at
//! the cost of recent ones would be worse than useless.

/// Ceiling on how many recent turns are protected from eviction.
///
/// Bounded so a pathological history cannot pin an unbounded tail: the point is to
/// keep the current exchange coherent, not to exempt it from the budget entirely.
pub const MAX_PROTECTED_TURNS: usize = 8;

/// Default number of recent turns protected when no policy is configured.
///
/// Four is two full exchanges. Below that the model tends to lose the user's
/// actual question; above it, old turns start crowding out the system prompt.
pub const DEFAULT_KEEP_LAST_TURNS: usize = 4;

/// What the window decided, for the response and the trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowReport {
    /// Turns sent to the model.
    pub kept: usize,
    /// Turns dropped.
    pub dropped: usize,
    /// True when anything was dropped, i.e. the model did not see the full history.
    pub truncated: bool,
    /// True when the keep-last floor forced turns to be kept past the budget.
    ///
    /// Distinct from `truncated` on purpose: one says "history was cut", the other
    /// says "the cut went further than the budget alone would have taken". A user
    /// debugging a model that ignores the recent turn needs to tell them apart.
    pub over_budget: bool,
    /// Prompt tokens attributed to the turns that were dropped. Measured, not
    /// estimated by counting characters.
    pub dropped_tokens: usize,
}

/// How to bound one turn's history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowPolicy {
    /// Total prompt budget in tokens, after the completion reserve.
    pub history_budget: usize,
    /// Recent turns protected from eviction regardless of budget.
    pub keep_last_turns: usize,
}

impl WindowPolicy {
    /// Builds a policy from a context size, a reply allowance and a reserve.
    ///
    /// `reserve` is subtracted before history is budgeted, so a turn cannot consume
    /// the space the reply needs. Clamped so a misconfigured context size cannot
    /// produce a zero or negative budget -- a zero budget would silently drop all
    /// history and look like a working window.
    #[cfg(test)]
    pub fn new(context_tokens: usize, reply_tokens: usize, reserve_tokens: usize) -> Self {
        let reply = reply_tokens.max(1);
        let reserve = reserve_tokens.max(reply);
        let history_budget = context_tokens.saturating_sub(reserve).max(reply);
        Self {
            history_budget,
            keep_last_turns: DEFAULT_KEEP_LAST_TURNS,
        }
    }

    /// Overrides the keep-last floor.
    ///
    /// Used by the tests to pin an exact budget; the hot path constructs the policy
    /// directly because it already holds a clamped value from `keep_last_turns()`.
    #[cfg(test)]
    pub fn with_keep_last(mut self, turns: usize) -> Self {
        self.keep_last_turns = turns.min(MAX_PROTECTED_TURNS);
        self
    }
}

/// One turn's cost, as measured by the caller's tokenizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// `"user"` or `"assistant"`.
    pub role: String,
    /// Token cost of this turn's content.
    pub tokens: usize,
}

/// Token-cost estimate used when the real tokenizer is unavailable.
///
/// Plain whitespace counting, which is what this file did before. It **undercounts**:
/// measured against the running server's `/tokenize`, a typical English sentence
/// tokenizes to ~481 tokens against a 420-word whitespace estimate -- about 15% low.
///
/// Undercounting is the dangerous direction. A budget built on it admits prompts the
/// model then rejects outright, and the user sees an HTTP 400 instead of an answer.
/// A conservative estimate that occasionally trims one turn early is far cheaper than
/// that.
///
/// So: whitespace words, plus a per-character allowance for the sub-word pieces the
/// whitespace count cannot see. 15% of the character count, which is comfortably above
/// the measured 15% shortfall while staying nowhere near a character-per-token worst
/// case that would truncate every conversation to nothing.
pub fn conservative_token_estimate(text: &str) -> usize {
    let words = text.split_whitespace().count();
    let chars = text.chars().count();
    let total = words + chars * 15 / 100;
    total.max(1)
}

/// The prompt-token budget for one turn's history.
///
/// Three inputs have to agree, and historically only two did:
///
/// * `configured` -- the user's `conversation_token_limit` preference.
/// * `reserve` -- room for the reply, which the setting never subtracted.
/// * `actual_ctx` -- the context the running llama-server was launched with, derived
///   from VRAM and model size and often far smaller than the setting. On this machine
///   a 3B model runs at 8192 ctx while the setting was 16384.
///
/// Budgeting against the setting alone means long turns are trimmed to a size the
/// model still rejects. Observed live, verbatim from llama-server:
///
/// ```text
/// request (18848 tokens) exceeds the available context size (8192 tokens)
/// ```
///
/// That is a 400, not a degraded answer, and no amount of trimming fixes it while the
/// ceiling sits above the real one. So the budget is clamped to the context the model
/// is actually using -- the only number that bounds the request.
pub fn history_budget(configured: usize, reserve: usize, actual_ctx: u32) -> usize {
    let ceiling = if actual_ctx == 0 {
        configured
    } else {
        configured.min(actual_ctx as usize)
    };
    ceiling.saturating_sub(reserve).max(1)
}

/// Bounds a list of turns to fit a token budget, newest first.
///
/// Returns the *suffix* of `turns` whose assembled text fits `budget`, always including
/// at least the newest turn.
///
/// This exists because a per-item cap is not a total cap. A summarizer that clipped
/// each turn to 2000 characters still assembled 80 x 2000 chars for a trim that
/// dropped 80 turns, which reached llama-server as a 21,485-token request against an
/// 8192 context and was rejected outright. The rejection was silent to the user: the
/// summarization runs detached, so they got an answer and lost the summary.
///
/// Newest-first because that is where the decisions worth preserving are.
pub fn bound_turns<T, F>(turns: &[T], budget: usize, cost: F) -> &[T]
where
    F: Fn(&T) -> usize,
{
    if turns.is_empty() {
        return turns;
    }
    let mut used = 0usize;
    let mut start = turns.len();
    for (i, turn) in turns.iter().enumerate().rev() {
        let c = cost(turn).max(1);
        if i == turns.len() - 1 {
            // The newest turn always survives. A bounded prompt containing nothing is
            // worse than one containing too much.
            used += c;
            start = i;
            continue;
        }
        if used + c > budget {
            break;
        }
        used += c;
        start = i;
    }
    &turns[start..]
}

/// Applies the sliding window.
///
/// `turns` is the history *excluding* the turn about to be sent, oldest first.
///
/// Walks newest-to-oldest accumulating cost until the budget is exhausted, then
/// keeps walking for up to `keep_last_turns` turns so the recent exchange survives
/// intact. Walking in that direction is what makes the floor cheap: the protected
/// turns are exactly the ones already visited.
pub fn apply(turns: &[Turn], policy: WindowPolicy) -> (Vec<Turn>, WindowReport) {
    let mut report = WindowReport::default();
    if turns.is_empty() {
        return (Vec::new(), report);
    }

    let mut used = 0usize;
    let mut keep_from = turns.len(); // index of the oldest kept turn
    let mut over_budget = false;

    for (i, turn) in turns.iter().enumerate().rev() {
        let cost = turn.tokens.max(1);
        let within_budget = used + cost <= policy.history_budget;
        let protected = turns.len() - i <= policy.keep_last_turns;

        if within_budget {
            used += cost;
            keep_from = i;
        } else if protected {
            // Budget spent, but this turn is part of the protected recent exchange.
            // Kept anyway and flagged, because a silently over-budget prompt is
            // worse than a reported one.
            used += cost;
            keep_from = i;
            over_budget = true;
        } else {
            // Old enough to be expendable and no longer affordable: everything
            // before this point goes too.
            break;
        }
    }

    let kept: Vec<Turn> = turns[keep_from..].to_vec();
    report.kept = kept.len();
    report.dropped = turns.len() - kept.len();
    report.truncated = report.dropped > 0;
    report.over_budget = over_budget;
    report.dropped_tokens = turns[..keep_from].iter().map(|t| t.tokens).sum();
    (kept, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds alternating user/assistant turns from a list of token costs.
    fn turns(costs: &[usize]) -> Vec<Turn> {
        costs
            .iter()
            .enumerate()
            .map(|(i, &tokens)| Turn {
                role: if i % 2 == 0 {
                    "user".into()
                } else {
                    "assistant".into()
                },
                tokens,
            })
            .collect()
    }

    #[test]
    fn an_empty_history_is_untouched() {
        let (kept, r) = apply(&[], WindowPolicy::new(4096, 512, 1024));
        assert!(kept.is_empty());
        assert!(!r.truncated);
        assert_eq!(r.dropped, 0);
    }

    #[test]
    fn a_history_that_fits_is_kept_whole() {
        let t = turns(&[100, 90, 80, 70]);
        let (kept, r) = apply(&t, WindowPolicy::new(4096, 512, 1024).with_keep_last(0));
        assert_eq!(kept.len(), 4);
        assert!(!r.truncated);
        assert_eq!(r.dropped_tokens, 0);
    }

    #[test]
    fn old_turns_are_dropped_before_recent_ones() {
        // Budget fits the last three but not the first.
        let t = turns(&[500, 100, 100, 100]);
        let (kept, r) = apply(
            &t,
            WindowPolicy {
                history_budget: 300,
                keep_last_turns: 0,
            },
        );
        assert_eq!(kept.len(), 3, "the oldest turn should go first");
        assert_eq!(kept[0].tokens, 100);
        assert!(r.truncated);
        assert_eq!(r.dropped, 1);
        assert_eq!(r.dropped_tokens, 500);
    }

    #[test]
    fn the_recent_exchange_survives_even_when_it_exceeds_the_budget() {
        // The case a plain budget would get wrong: the newest turns cost more than
        // the whole allowance. Dropping them is the one outcome that makes a
        // conversation incoherent, so they are kept and the overflow is reported.
        let t = turns(&[10, 400, 400, 400]);
        // keep_last_turns = 3 protects exactly the newest three, which together
        // cost 1200 against a budget of 100 -- so all three survive and the
        // overflow is reported rather than silently dropping the user's own turn.
        let (kept, r) = apply(
            &t,
            WindowPolicy {
                history_budget: 100,
                keep_last_turns: 3,
            },
        );
        assert_eq!(kept.len(), 3, "the protected tail must survive");
        assert_eq!(
            kept[0].tokens, 400,
            "the protected window starts at the oldest protected turn"
        );
        assert!(r.over_budget, "overflow must be reported, not silent");
        assert!(r.truncated);
    }

    #[test]
    fn the_floor_never_protects_unbounded_history() {
        // Without the MAX_PROTECTED_TURNS clamp, keep_last=1000 would exempt
        // everything and the budget would stop meaning anything.
        let t = turns(&vec![200usize; 40]);
        let policy = WindowPolicy::new(300, 50, 0).with_keep_last(1000);
        assert_eq!(policy.keep_last_turns, MAX_PROTECTED_TURNS);
        let (kept, _) = apply(&t, policy);
        assert!(
            kept.len() <= MAX_PROTECTED_TURNS + 1,
            "protected tail was {} turns",
            kept.len()
        );
    }

    #[test]
    fn a_zero_budget_cannot_silently_drop_all_history() {
        // A context size smaller than the reserve would otherwise produce a zero
        // budget, which reads as a working window that keeps nothing.
        let policy = WindowPolicy::new(100, 512, 4096);
        assert!(policy.history_budget >= 512, "budget must stay usable");
    }

    #[test]
    fn the_reserve_actually_reduces_the_history_budget() {
        let full = WindowPolicy::new(8192, 512, 0);
        let reserved = WindowPolicy::new(8192, 512, 1024);
        assert_eq!(full.history_budget, 8192 - 512);
        assert!(
            reserved.history_budget < full.history_budget,
            "a reserve must cost history something"
        );
    }

    #[test]
    fn the_reserve_defaults_to_at_least_the_reply() {
        // A zero reserve would reintroduce the original hole.
        let policy = WindowPolicy::new(4096, 512, 0);
        assert!(policy.history_budget <= 4096 - 512);
    }

    #[test]
    fn dropped_tokens_are_counted_from_what_was_actually_dropped() {
        let t = turns(&[100, 200, 300, 400]);
        let (kept, r) = apply(
            &t,
            WindowPolicy {
                history_budget: 400,
                keep_last_turns: 0,
            },
        );
        let dropped: usize = t.len() - kept.len();
        assert_eq!(r.dropped, dropped);
        assert_eq!(
            r.dropped_tokens,
            t[..dropped].iter().map(|x| x.tokens).sum::<usize>()
        );
    }

    #[test]
    fn a_zero_token_turn_is_treated_as_costing_something() {
        // An empty turn would otherwise be free and could be kept indefinitely.
        let t = vec![
            Turn {
                role: "user".into(),
                tokens: 0,
            },
            Turn {
                role: "assistant".into(),
                tokens: 0,
            },
        ];
        // Each turn is charged its minimum of 1, so a budget of 1 admits one and
        let (kept, r) = apply(
            &t,
            WindowPolicy {
                history_budget: 1,
                keep_last_turns: 0,
            },
        );
        assert_eq!(kept.len(), 1, "a zero-token turn still costs something");
        assert!(r.truncated);
    }

    #[test]
    fn order_is_preserved() {
        let t = turns(&[90, 80, 70, 60, 50]);
        let (kept, _) = apply(
            &t,
            WindowPolicy {
                history_budget: 150,
                keep_last_turns: 1,
            },
        );
        let kept_tokens: Vec<usize> = kept.iter().map(|x| x.tokens).collect();
        let original: Vec<usize> = t.iter().map(|x| x.tokens).collect();
        let start = original.len() - kept_tokens.len();
        assert_eq!(
            kept_tokens,
            original[start..],
            "must stay a contiguous suffix"
        );
    }

    #[test]
    fn history_budget_is_clamped_to_the_context_the_model_actually_runs() {
        // The live failure this exists to prevent: `conversation_token_limit` was
        // 16384 while the 3B model runs at 8192 ctx, so every long turn was trimmed to
        // a size llama-server still rejected with
        // "request (18848 tokens) exceeds the available context size (8192 tokens)".
        let b = history_budget(16384, 1024, 8192);
        assert!(
            b <= 8192,
            "budget {b} exceeds the real context; long turns would 400"
        );
        assert_eq!(b, 7168);
    }

    #[test]
    fn an_unknown_running_context_leaves_the_setting_alone() {
        // No server started yet, or one started elsewhere. Falling back to the
        // configured ceiling beats guessing a context size and under-budgeting every
        // conversation.
        assert_eq!(history_budget(16384, 1024, 0), 15360);
    }

    #[test]
    fn a_setting_below_the_real_context_is_respected() {
        // The clamp must not inflate a deliberately small budget.
        assert_eq!(history_budget(2048, 512, 8192), 1536);
    }

    #[test]
    fn the_reserve_is_subtracted_before_the_ceiling_is_applied() {
        // Both constraints must hold: never above the context, always room to reply.
        let b = history_budget(8192, 2048, 8192);
        assert_eq!(b, 6144);
        assert!(b + 2048 <= 8192);
    }

    #[test]
    fn a_reserve_larger_than_the_context_still_yields_a_usable_budget() {
        // Misconfiguration must not produce 0, which would silently drop all history
        // and look like a working window.
        let b = history_budget(1024, 4096, 1024);
        assert!(b >= 1, "budget collapsed to {b}");
    }

    #[test]
    fn the_keep_last_floor_stays_bounded() {
        // The env knob the audit could not find anywhere in the codebase.
        assert_eq!(DEFAULT_KEEP_LAST_TURNS, 4);
        assert_eq!(
            MAX_PROTECTED_TURNS, 8,
            "the floor must stay bounded or the budget stops meaning anything"
        );
    }

    #[test]
    fn the_fallback_estimate_never_undercounts_real_tokens() {
        // Measured live: 481 real tokens vs a 420-word whitespace count (~15% low).
        // The estimator has to land at or above the real figure or the whole budget is
        // optimistic, and every long turn 400s.
        let text = "explain the deploy pipeline in exhaustive detail. ".repeat(60);
        let ws = text.split_whitespace().count();
        let est = conservative_token_estimate(&text);
        assert_eq!(ws, 420, "the whitespace count this replaces is 420");
        assert!(
            est >= 481,
            "estimate {est} is below the 481 real tokens; it must be conservative"
        );
    }

    #[test]
    fn the_fallback_estimate_never_returns_zero() {
        // An empty turn still has to cost something, or it is kept forever.
        assert_eq!(conservative_token_estimate(""), 1);
    }

    #[test]
    fn the_fallback_estimate_handles_short_text() {
        // The character allowance is a fraction, so it vanishes on very short input.
        // That is fine -- "hi" really is ~1 token -- and the `.max(1)` keeps it from
        // being free. Pinned because a change that made short turns cost 0 would let
        // the window keep them unboundedly.
        assert_eq!(conservative_token_estimate("hi"), 1);
        // 3 words + 13 chars * 15% = 3 + 1.
        assert_eq!(conservative_token_estimate("one two three"), 4);
        // Monotonic in length, which is the property the budget actually depends on.
        assert!(
            conservative_token_estimate(&"word ".repeat(200))
                > conservative_token_estimate(&"word ".repeat(100))
        );
    }

    #[test]
    fn bounded_turns_drops_the_oldest_when_over_budget() {
        // 4 turns of 100 against a budget of 250 admits two, not three.
        let t: Vec<usize> = vec![100, 100, 100, 100];
        let kept = bound_turns(&t, 250, |x| *x);
        assert_eq!(kept, &[100, 100], "oldest goes first");
        assert_eq!(kept.iter().sum::<usize>(), 200);
    }

    #[test]
    fn bounded_turns_keeps_everything_that_fits() {
        let t: Vec<usize> = vec![10, 20, 30];
        assert_eq!(bound_turns(&t, 100, |x| *x), &t[..]);
    }

    #[test]
    fn bounded_turns_always_keeps_the_newest_even_when_alone_it_overflows() {
        // The case that silently lost summaries: a single turn larger than the whole
        // budget. Dropping it would send an empty prompt, and there is nothing to
        // summarize -- so it stays and the caller truncates the text.
        let t: Vec<usize> = vec![10, 10_000];
        let kept = bound_turns(&t, 100, |x| *x);
        assert_eq!(kept, &[10_000]);
    }

    #[test]
    fn bounded_turns_is_empty_in_and_empty_out() {
        let t: Vec<usize> = vec![];
        assert!(bound_turns(&t, 100, |x| *x).is_empty());
    }

    #[test]
    fn eighty_large_turns_fit_inside_a_small_budget() {
        // The live failure, reproduced in miniature: 80 turns of ~2000 chars each.
        // Before the total bound this assembled ~21,485 real tokens.
        let t: Vec<String> = (0..80)
            .map(|i| {
                format!(
                    "turn {i}: {}",
                    "explain the deploy pipeline in detail. ".repeat(45)
                )
            })
            .collect();
        let kept = bound_turns(&t, 2048, |s| conservative_token_estimate(s));
        let total: usize = kept.iter().map(|s| conservative_token_estimate(s)).sum();
        assert!(total <= 2048 * 2, "bound overshot: {total} tokens");
        assert!(kept.len() < 80, "nothing was dropped");
    }

    #[test]
    fn bounded_turns_keeps_the_newest_marked_turn() {
        let t: Vec<String> = (0..80).map(|i| format!("MARKER-{i}")).collect();
        let kept = bound_turns(&t, 40, |s| s.len());
        assert!(
            kept.iter().any(|s| s == "MARKER-79"),
            "the newest turn must survive"
        );
        assert!(
            !kept.iter().any(|s| s == "MARKER-0"),
            "the oldest should have been dropped"
        );
    }
}
