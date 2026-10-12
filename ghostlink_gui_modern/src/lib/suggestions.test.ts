import { describe, it, expect } from 'vitest';
import { deriveFollowUps, MAX_SUGGESTIONS } from './suggestions';

/**
 * The negative cases dominate on purpose. A chip that fires when it shouldn't
 * is worse than a missing one -- it trains the user to ignore the surface, and
 * an ignored suggestion row is strictly worse than no suggestion row. So for
 * every rule there is an assertion that it stays silent on the neighbouring
 * "almost" inputs, not just that it fires on the exact one.
 */
describe('deriveFollowUps', () => {
  describe('the empty turn', () => {
    it('offers nothing for a plain answer with no signal', () => {
      expect(deriveFollowUps({})).toEqual([]);
    });

    it('offers nothing when every count is zero and every flag false', () => {
      expect(
        deriveFollowUps({
          tools_run: 0,
          recalled_memories: 0,
          recalled_documents: 0,
          action_claim_corrected: false,
          truncated: false,
          summarized_history: false,
        })
      ).toEqual([]);
    });

    it('offers nothing with no ambient context either', () => {
      expect(deriveFollowUps({}, {})).toEqual([]);
    });
  });

  describe('corrected action-claim', () => {
    it('leads with the correction and offers a re-ask', () => {
      const s = deriveFollowUps({ action_claim_corrected: true });
      expect(s[0].id).toBe('corrected-claim');
      expect(s[0].text).toMatch(/no tool actually ran/i);
    });

    it('stays silent when the flag is false', () => {
      expect(deriveFollowUps({ action_claim_corrected: false })).toEqual([]);
    });

    it('stays silent when the flag is absent', () => {
      // Absent is the common case and must not read as corrected.
      expect(deriveFollowUps({ tools_run: 2 })).not.toContainEqual(
        expect.objectContaining({ id: 'corrected-claim' })
      );
    });
  });

  describe('tool calls', () => {
    it('offers to explain when a tool ran', () => {
      const s = deriveFollowUps({ tools_run: 1 });
      expect(s.map((x) => x.id)).toContain('explain-tools');
    });

    it('uses the singular for exactly one tool', () => {
      const s = deriveFollowUps({ tools_run: 1 });
      const chip = s.find((x) => x.id === 'explain-tools')!;
      expect(chip.text).toMatch(/1 tool call\b/);
      expect(chip.text).not.toMatch(/calls/);
    });

    it('uses the plural for more than one', () => {
      const s = deriveFollowUps({ tools_run: 3 });
      const chip = s.find((x) => x.id === 'explain-tools')!;
      expect(chip.text).toMatch(/3 tool calls/);
    });

    it('stays silent at zero', () => {
      expect(deriveFollowUps({ tools_run: 0 }).map((x) => x.id)).not.toContain('explain-tools');
    });
  });

  describe('recall', () => {
    it('offers to go deeper when memories were recalled', () => {
      const s = deriveFollowUps({ recalled_memories: 2 });
      expect(s.map((x) => x.id)).toContain('recall-more');
    });

    it('offers to go deeper when documents were recalled', () => {
      const s = deriveFollowUps({ recalled_documents: 1 });
      expect(s.map((x) => x.id)).toContain('recall-more');
    });

    it('sums memories and documents into one source count', () => {
      const s = deriveFollowUps({ recalled_memories: 1, recalled_documents: 2 });
      const chip = s.find((x) => x.id === 'recall-more')!;
      expect(chip.text).toMatch(/3 sources/);
    });

    it('uses the singular for exactly one source', () => {
      const s = deriveFollowUps({ recalled_memories: 1 });
      const chip = s.find((x) => x.id === 'recall-more')!;
      expect(chip.text).toMatch(/1 source\b/);
      expect(chip.text).not.toMatch(/sources/);
    });

    it('stays silent when both counts are zero', () => {
      expect(
        deriveFollowUps({ recalled_memories: 0, recalled_documents: 0 }).map((x) => x.id)
      ).not.toContain('recall-more');
    });
  });

  describe('condensed memory', () => {
    it('offers a recap when history was truncated', () => {
      expect(deriveFollowUps({ truncated: true }).map((x) => x.id)).toContain('condensed-memory');
    });

    it('offers a recap when a summary is in play', () => {
      expect(deriveFollowUps({ summarized_history: true }).map((x) => x.id)).toContain(
        'condensed-memory'
      );
    });

    it('emits only one recap chip when both are true', () => {
      const s = deriveFollowUps({ truncated: true, summarized_history: true });
      expect(s.filter((x) => x.id === 'condensed-memory')).toHaveLength(1);
    });

    it('stays silent when neither is set', () => {
      expect(deriveFollowUps({ truncated: false, summarized_history: false }).map((x) => x.id))
        .not.toContain('condensed-memory');
    });
  });

  describe('ambient task and review state', () => {
    it('offers the review walkthrough when a packet is pending', () => {
      const s = deriveFollowUps({}, { hasPendingReview: true });
      expect(s.map((x) => x.id)).toContain('review-pending');
    });

    it('offers the running-task check when a task is active', () => {
      const s = deriveFollowUps({}, { hasRunningTask: true });
      expect(s.map((x) => x.id)).toContain('task-running');
    });

    it('prefers the pending review over the running task when both hold', () => {
      const s = deriveFollowUps({}, { hasPendingReview: true, hasRunningTask: true });
      expect(s.map((x) => x.id)).toContain('review-pending');
      expect(s.map((x) => x.id)).not.toContain('task-running');
    });

    it('stays silent when neither state exists', () => {
      expect(deriveFollowUps({}, { hasRunningTask: false, hasPendingReview: false })).toEqual([]);
    });
  });

  describe('shape and limits', () => {
    it('never exceeds the maximum', () => {
      const s = deriveFollowUps(
        {
          action_claim_corrected: true,
          tools_run: 4,
          recalled_memories: 3,
          recalled_documents: 2,
          truncated: true,
        },
        { hasPendingReview: true }
      );
      expect(s.length).toBeLessThanOrEqual(MAX_SUGGESTIONS);
    });

    it('every chip carries a non-empty id, text and label', () => {
      const s = deriveFollowUps(
        { action_claim_corrected: true, tools_run: 2, recalled_memories: 1 },
        { hasPendingReview: true }
      );
      for (const chip of s) {
        expect(chip.id.length).toBeGreaterThan(0);
        expect(chip.text.length).toBeGreaterThan(0);
        expect(chip.label.length).toBeGreaterThan(0);
      }
    });

    it('never emits a duplicate id', () => {
      const s = deriveFollowUps(
        { tools_run: 2, recalled_memories: 1, recalled_documents: 1, truncated: true },
        { hasPendingReview: true }
      );
      const ids = s.map((x) => x.id);
      expect(new Set(ids).size).toBe(ids.length);
    });

    it('is deterministic for the same input', () => {
      const signals = { tools_run: 2, recalled_memories: 1 };
      expect(deriveFollowUps(signals)).toEqual(deriveFollowUps(signals));
    });
  });
});
