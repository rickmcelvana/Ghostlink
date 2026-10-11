/**
 * Contextual follow-up suggestions for the chat surface.
 *
 * Pure and client-side: `deriveFollowUps` takes the per-turn metadata a chat
 * response carries plus a little ambient client context, and returns the
 * follow-ups worth offering. No network call, no model call, no latency on the
 * hot path -- everything it reads is already computed by the time the answer
 * finishes rendering.
 *
 * The governing rule is that a suggestion which fires when it shouldn't is
 * worse than one that never appears. A chip the user learns to ignore is
 * noise, and noise trains people to dismiss the surface entirely. So every
 * rule keys off a concrete, positive signal and the negative cases are the
 * heavily-tested half of `suggestions.test.ts`.
 */

/** Per-turn fields a chat response carries. Mirrors the backend's response. */
export interface TurnSignals {
  /** Number of MCP tools actually executed this turn. */
  tools_run?: number;
  /** Memories the proactive retriever surfaced this turn. */
  recalled_memories?: number;
  /** Documents the RAG retriever surfaced this turn. */
  recalled_documents?: number;
  /** True when the server rewrote a reply that claimed an action no tool performed. */
  action_claim_corrected?: boolean;
  /** True when older turns were dropped to fit the context window. */
  truncated?: boolean;
  /** True when the session is working from a condensed summary rather than the full transcript. */
  summarized_history?: boolean;
  /** Server-generated title, present on a session's first turn. */
  session_title?: string;
}

/** Ambient client state a suggestion may reasonably key off. */
export interface SuggestionContext {
  /** The model that served this turn, if known. */
  model?: string;
  /** True when a task agent run exists and is still active. */
  hasRunningTask?: boolean;
  /** True when a ReviewPacket is awaiting a human decision. */
  hasPendingReview?: boolean;
}

export interface Suggestion {
  /** Stable identity -- used as the React key and to dedupe. */
  id: string;
  /** The prompt text sent when the chip is clicked. */
  text: string;
  /** Accessible label; screen readers announce this instead of `text`. */
  label: string;
}

/** Most chips shown at once. Three is a scannable row; more becomes a wall. */
export const MAX_SUGGESTIONS = 3;

/**
 * Derive follow-up suggestions from a completed turn.
 *
 * Returns at most `MAX_SUGGESTIONS`, ordered most- to least-specific. An
 * empty array is a normal, common result -- a plain answer with no tool calls
 * and no retrieval earns no follow-ups, and inventing one would be noise.
 */
export function deriveFollowUps(
  signals: TurnSignals,
  context: SuggestionContext = {}
): Suggestion[] {
  const out: Suggestion[] = [];
  const seen = new Set<string>();

  const push = (id: string, text: string, label: string) => {
    if (seen.has(id)) return;
    seen.add(id);
    out.push({ id, text, label });
  };

  // A corrected action-claim is the highest-value follow-up: the model
  // asserted it did something it did not do, the server rewrote the reply, and
  // the user deserves an immediate, obvious way to re-ask. Lead with it.
  if (signals.action_claim_corrected === true) {
    push(
      'corrected-claim',
      'That correction means no tool actually ran. Re-ask with the action spelled out.',
      'Re-ask, noting the previous reply claimed an action that never happened'
    );
  }

  // Tools ran. Offer to explain what came back -- the single most common
  // follow-up after a tool-backed answer, and one the model can answer from
  // the same turn's results.
  if ((signals.tools_run ?? 0) > 0) {
    push(
      'explain-tools',
      `Explain what those ${signals.tools_run} tool call${signals.tools_run === 1 ? '' : 's'} returned.`,
      'Ask the assistant to explain the tool results from this turn'
    );
  }

  // Retrieval surfaced something. Offer to go deeper rather than leaving the
  // user to guess what was remembered.
  const recalled = (signals.recalled_memories ?? 0) + (signals.recalled_documents ?? 0);
  if (recalled > 0) {
    push(
      'recall-more',
      `What else do you remember that relates to this? (${recalled} source${recalled === 1 ? '' : 's'} used)`,
      'Ask for more recalled context related to this turn'
    );
  }

  // Working from a condensed memory. Make the truncation visible and offer a
  // recap -- otherwise the model looks like it forgot on its own.
  if (signals.truncated === true || signals.summarized_history === true) {
    push(
      'condensed-memory',
      'You are working from a condensed memory. Recap the key points in full.',
      'Ask for a recap, noting the conversation history was condensed'
    );
  }

  // Ambient task/review state: these are actions, not questions, and they are
  // only offered when the state genuinely exists.
  if (context.hasPendingReview === true) {
    push(
      'review-pending',
      'Walk me through the proposed changes and what verification found.',
      'Review the pending proposed changes'
    );
  } else if (context.hasRunningTask === true) {
    push(
      'task-running',
      'What is the task agent doing right now?',
      'Ask about the running task agent'
    );
  }

  return out.slice(0, MAX_SUGGESTIONS);
}
