//! The bounded agent loop's budget, and the stop reasons that end a turn.
//!
//! Phase 4 of the local assistant. The tool loops previously threaded a bare
//! `iterations_left: usize` — one counter, decremented once per round trip, with
//! no way to express "at most N tool calls" or "at most M tokens". That made it
//! impossible to bound a turn by cost, which is the constraint that matters when
//! the model is stuck in a loop: an iteration cap alone doesn't stop a single
//! iteration from being expensive.
//!
//! [`Budget`] replaces that with three independent limits and reports which one
//! ended the turn, so the model and the operator are told *why* it stopped rather
//! than getting a generic bailout message.

use std::time::Duration;

/// Default per-turn limits.
///
/// `max_steps` intentionally matches the old `MAX_TOOL_ITERATIONS` of 6: that
/// number was already tuned (raised from 3 on 2026-08-10 because 3 cut off
/// legitimate multi-step tasks), and changing it here would silently alter
/// behavior for every existing deployment. The rationale for 6 lives on
/// `mcp::toolcall::MAX_TOOL_ITERATIONS`; this is the same limit expressed as a
/// budget, not a new policy.
#[derive(Debug, Clone, PartialEq)]
pub struct Budget {
    /// Plan/act/observe cycles, including the final answer-producing one.
    pub max_steps: usize,
    /// Individual tool dispatches across the whole turn.
    pub max_tool_calls: usize,
    /// Cumulative generation tokens across the whole turn.
    pub max_tokens: u32,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_steps: crate::mcp::toolcall::MAX_TOOL_ITERATIONS,
            // Six steps can each request several calls (Ollama and vLLM do
            // batch them), so this is deliberately a little above the step cap
            // rather than equal to it.
            max_tool_calls: 12,
            // Bounds a runaway turn's cost. Well above a normal multi-step
            // answer, low enough that a stuck loop terminates.
            max_tokens: 8192,
        }
    }
}

impl Budget {
    /// Builds a budget, clamping every limit to at least one.
    ///
    /// The clamp lives here rather than only in [`Budget::from_env`] so a
    /// programmatically-constructed budget can't produce a turn that can never
    /// take a step — which would look like an instant, silent failure.
    pub fn new(max_steps: usize, max_tool_calls: usize, max_tokens: u32) -> Self {
        Self {
            max_steps: max_steps.max(1),
            max_tool_calls: max_tool_calls.max(1),
            max_tokens: max_tokens.max(1),
        }
    }

    /// Reads limits from the environment, falling back to the defaults.
    ///
    /// Each is clamped to at least 1 so a zero or unparseable value can't
    /// produce a turn that can never take a step.
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            max_steps: env_usize("GHOSTLINK_AGENT_MAX_STEPS", d.max_steps),
            max_tool_calls: env_usize("GHOSTLINK_AGENT_MAX_TOOL_CALLS", d.max_tool_calls),
            max_tokens: env_u32("GHOSTLINK_AGENT_MAX_TOKENS", d.max_tokens),
        }
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
        .max(1)
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(default)
        .max(1)
}

/// Live budget consumption for one turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BudgetSpend {
    steps_used: usize,
    tool_calls_used: usize,
    tokens_used: u32,
    /// Set when a tool-call batch was refused for exceeding the cap.
    ///
    /// Tracked explicitly rather than inferred from `tool_calls_used`: a refused
    /// batch leaves the counter *below* the cap (4 of 5, say), so the caller
    /// would otherwise get `None` and have no way to know it must stop.
    tool_calls_blocked: bool,
}

/// Why a turn stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The model produced a final answer.
    FinalAnswer,
    /// A step budget ran out.
    StepsExhausted,
    /// The tool-call budget ran out.
    ToolCallsExhausted,
    /// The token budget ran out.
    TokensExhausted,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FinalAnswer => "final_answer",
            Self::StepsExhausted => "steps_exhausted",
            Self::ToolCallsExhausted => "tool_calls_exhausted",
            Self::TokensExhausted => "tokens_exhausted",
        }
    }

    /// Classifies a turn that ended without further tool calls.
    ///
    /// Kept as a function rather than letting callers compare against
    /// `FinalAnswer` inline so the "did it finish, or did a limit stop it?"
    /// question has one answer in one place.
    pub fn classify_end(reason: Option<StopReason>) -> StopReason {
        reason.unwrap_or(StopReason::FinalAnswer)
    }

    /// Whether the turn ended because a limit was hit, rather than finishing.
    pub fn is_exhausted(self) -> bool {
        matches!(
            self,
            Self::StepsExhausted | Self::ToolCallsExhausted | Self::TokensExhausted
        )
    }
}

/// Tracks budget consumption and decides when the turn must stop.
///
/// Ordered deliberately: a step is only permitted if the *other* limits also
/// leave room, so a turn never starts a step it cannot afford to finish. The
/// token check comes last because a step's cost isn't known until after it runs,
/// which is why it is a post-hoc check on the total rather than a pre-check.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetTracker {
    budget: Budget,
    spend: BudgetSpend,
}

impl BudgetTracker {
    pub fn new(budget: Budget) -> Self {
        Self {
            budget,
            spend: BudgetSpend::default(),
        }
    }

    /// Whether another step may begin.
    pub fn can_step(&self) -> bool {
        !self.spend.tool_calls_blocked
            && self.spend.steps_used < self.budget.max_steps
            && self.spend.tool_calls_used < self.budget.max_tool_calls
            && self.spend.tokens_used < self.budget.max_tokens
    }

    /// Records that a step is starting. Returns `false` (and records nothing)
    /// when no budget remains.
    pub fn begin_step(&mut self) -> bool {
        if !self.can_step() {
            return false;
        }
        self.spend.steps_used += 1;
        true
    }

    /// Records a step's token cost.
    pub fn charge_tokens(&mut self, tokens: u32) {
        // Saturating: a misbehaving backend reporting u32::MAX shouldn't wrap
        // the accumulator back to a small number and silently un-exhaust the
        // budget.
        self.spend.tokens_used = self.spend.tokens_used.saturating_add(tokens);
    }

    /// Records a batch of tool calls. Returns `false` when the batch would
    /// exceed the tool-call budget, in which case nothing is recorded and the
    /// caller must not dispatch the batch.
    ///
    /// All-or-nothing on purpose: dispatching "as many as fit" would silently
    /// drop the tail of a batch the model asked for, and a model that saw a
    /// partial result set could reasonably conclude the rest returned nothing.
    pub fn charge_tool_calls(&mut self, count: usize) -> bool {
        if self.spend.tool_calls_used.saturating_add(count) > self.budget.max_tool_calls {
            self.spend.tool_calls_blocked = true;
            return false;
        }
        self.spend.tool_calls_used += count;
        true
    }

    /// The reason the turn must stop, or `None` while it may continue.
    ///
    /// Steps are reported first because that is the limit a stuck model hits
    /// most often, and the message is more actionable when it names the real
    /// cause.
    pub fn stop_reason(&self) -> Option<StopReason> {
        if self.spend.steps_used >= self.budget.max_steps {
            return Some(StopReason::StepsExhausted);
        }
        if self.spend.tool_calls_blocked || self.spend.tool_calls_used >= self.budget.max_tool_calls
        {
            return Some(StopReason::ToolCallsExhausted);
        }
        if self.spend.tokens_used >= self.budget.max_tokens {
            return Some(StopReason::TokensExhausted);
        }
        None
    }

    pub fn steps_used(&self) -> usize {
        self.spend.steps_used
    }

    pub fn tool_calls_used(&self) -> usize {
        self.spend.tool_calls_used
    }

    pub fn tokens_used(&self) -> u32 {
        self.spend.tokens_used
    }

    pub fn budget(&self) -> &Budget {
        &self.budget
    }
}

/// The observation shown to the model (and surfaced to the user) when a turn
/// ends because a budget ran out.
///
/// Names the limit and the actual consumption: "stopped after N tool round-trips"
/// was the old message and it gave the reader no way to tell whether to raise a
/// limit or fix the prompt.
pub fn exhaustion_message(reason: StopReason, tracker: &BudgetTracker) -> String {
    let b = tracker.budget();
    match reason {
        StopReason::StepsExhausted => format!(
            "(stopped: step budget of {} exhausted after {} step(s) and {} tool call(s); \
             raise GHOSTLINK_AGENT_MAX_STEPS if this task legitimately needs more)",
            b.max_steps,
            tracker.steps_used(),
            tracker.tool_calls_used()
        ),
        StopReason::ToolCallsExhausted => format!(
            "(stopped: tool-call budget of {} exhausted after {} call(s) across {} step(s); \
             raise GHOSTLINK_AGENT_MAX_TOOL_CALLS if this task legitimately needs more)",
            b.max_tool_calls,
            tracker.tool_calls_used(),
            tracker.steps_used()
        ),
        StopReason::TokensExhausted => format!(
            "(stopped: token budget of {} exhausted after {} token(s); \
             raise GHOSTLINK_AGENT_MAX_TOKENS if this task legitimately needs more)",
            b.max_tokens,
            tracker.tokens_used()
        ),
        other => format!("(stopped: {})", other.as_str()),
    }
}

/// Wall-clock ceiling for a single agent turn.
///
/// Not part of [`Budget`] because it bounds latency rather than cost, and the
/// two are tuned independently: a turn can be cheap but slow (a tool that
/// blocks) or expensive but fast. Defaults to 0 = disabled, because the engine
/// layers already have their own connect and first-token timeouts and this is a
/// backstop against a *composite* of steps overrunning, not a replacement.
pub fn turn_timeout() -> Option<Duration> {
    let secs = std::env::var("GHOSTLINK_AGENT_TURN_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    (secs > 0).then(|| Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> BudgetTracker {
        BudgetTracker::new(Budget::new(3, 5, 100))
    }

    #[test]
    fn default_step_cap_matches_the_legacy_iteration_limit() {
        // Changing this would silently alter behavior for existing deployments.
        assert_eq!(
            Budget::default().max_steps,
            crate::mcp::toolcall::MAX_TOOL_ITERATIONS
        );
    }

    #[test]
    fn a_fresh_tracker_can_step() {
        let mut t = tracker();
        assert!(t.can_step());
        assert!(t.begin_step());
        assert_eq!(t.steps_used(), 1);
    }

    #[test]
    fn steps_are_capped() {
        let mut t = tracker();
        for _ in 0..3 {
            assert!(t.begin_step(), "should allow up to max_steps");
        }
        assert!(!t.can_step());
        assert!(!t.begin_step(), "must refuse a step past the cap");
        assert_eq!(t.steps_used(), 3, "a refused step must not be counted");
    }

    #[test]
    fn tool_call_cap_stops_a_batch_that_would_overflow() {
        let mut t = tracker();
        assert!(t.charge_tool_calls(4));
        // 4 + 2 > 5, so the whole batch is refused rather than truncated.
        assert!(!t.charge_tool_calls(2));
        assert_eq!(
            t.tool_calls_used(),
            4,
            "a refused batch must not be counted"
        );
        assert_eq!(
            t.stop_reason(),
            Some(StopReason::ToolCallsExhausted),
            "a refused batch is still a stop"
        );
    }

    #[test]
    fn tokens_are_capped_and_saturate() {
        let mut t = tracker();
        t.charge_tokens(60);
        assert!(t.can_step());
        t.charge_tokens(40);
        assert_eq!(t.stop_reason(), Some(StopReason::TokensExhausted));
        // A misbehaving backend must not wrap the accumulator back to small.
        t.charge_tokens(u32::MAX);
        assert_eq!(t.tokens_used(), u32::MAX);
        assert_eq!(t.stop_reason(), Some(StopReason::TokensExhausted));
    }

    #[test]
    fn steps_are_reported_before_the_other_limits() {
        // A stuck model hits the step cap most often, and the more actionable
        // message is the one naming the real cause. Both limits are at their cap
        // here, so the precedence rule is what's being pinned.
        let mut t = BudgetTracker::new(Budget::new(2, 2, 10_000));
        // Both steps first, while the tool-call budget still has room --
        // otherwise begin_step is refused and the step count never reaches 2.
        t.begin_step();
        t.begin_step();
        t.charge_tool_calls(2); // now steps 2/2 and tool calls 2/2
        assert_eq!(t.stop_reason(), Some(StopReason::StepsExhausted));
    }

    #[test]
    fn a_refused_batch_stops_the_turn_even_though_spend_is_below_the_cap() {
        let mut t = BudgetTracker::new(Budget::new(9, 5, 10_000));
        assert!(t.charge_tool_calls(4));
        // 4 + 2 > 5: refused. Spend is 4/5, which is *not* exhausted on its own.
        assert!(!t.charge_tool_calls(2));
        assert_eq!(t.tool_calls_used(), 4);
        assert_eq!(t.stop_reason(), Some(StopReason::ToolCallsExhausted));
        assert!(!t.can_step(), "a refused batch must also halt stepping");
    }

    #[test]
    fn exhaustion_message_names_the_limit_and_the_actual_use() {
        let mut t = tracker();
        t.begin_step();
        t.begin_step();
        t.begin_step();
        let msg = exhaustion_message(StopReason::StepsExhausted, &t);
        assert!(msg.contains("step budget of 3"));
        assert!(msg.contains("3 step(s)"));
        assert!(msg.contains("GHOSTLINK_AGENT_MAX_STEPS"));
    }

    #[test]
    fn exhaustion_message_distinguishes_the_three_limits() {
        let mut calls = BudgetTracker::new(Budget::new(99, 1, 10_000));
        calls.charge_tool_calls(1);
        let msg = exhaustion_message(StopReason::ToolCallsExhausted, &calls);
        assert!(msg.contains("tool-call budget"));
        assert!(msg.contains("GHOSTLINK_AGENT_MAX_TOOL_CALLS"));

        let mut tokens = BudgetTracker::new(Budget::new(99, 99, 5));
        tokens.charge_tokens(5);
        let msg = exhaustion_message(StopReason::TokensExhausted, &tokens);
        assert!(msg.contains("token budget"));
        assert!(msg.contains("GHOSTLINK_AGENT_MAX_TOKENS"));
    }

    #[test]
    fn only_limit_reasons_count_as_exhausted() {
        assert!(StopReason::StepsExhausted.is_exhausted());
        assert!(StopReason::ToolCallsExhausted.is_exhausted());
        assert!(StopReason::TokensExhausted.is_exhausted());
        assert!(!StopReason::FinalAnswer.is_exhausted());
    }

    #[test]
    fn env_limits_are_clamped_to_at_least_one() {
        // A zero would produce a turn that can never take a step.
        let limiter = Budget::new(0, 0, 0);
        let mut t = BudgetTracker::new(limiter);
        assert!(t.begin_step(), "a zero budget must still permit one step");
        assert!(!t.begin_step());
    }
}
