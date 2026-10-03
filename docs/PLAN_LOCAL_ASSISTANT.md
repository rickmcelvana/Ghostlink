# Plan: Local Assistant Layer

Turns the existing chat + MCP tool loop into a bounded, capability-gated local
assistant. Derived from an internal brief; every claim below was checked against
the working tree, not assumed.

Status: **plan only** — no code written yet. Branch off `main` before starting.

---

## Ground truth (read before the phases)

Five facts from the tree that change the shape of the brief:

| Finding | Evidence | Consequence |
|---|---|---|
| A blocking approval flow already exists | `PendingToolCall` enum (`main.rs:4098`), `NativeLoopStep::NeedsConfirmation` (`main.rs:2598`), `POST /api/inference/chat/tool-confirm` | Item 3 is a reshape, not a greenfield build |
| Approval is per-*server*, not per-*tool* | `mcp_registry.requires_confirmation(&schema.server)` at `main.rs:2697`, `:2967`, `:3259` | read/write/exec needs a new layer *below* server granularity |
| Chats carry no workspace identity | Only the global `GHOSTLINK_WORKSPACE_ROOT` env (`main.rs:7278`); no `workspace_id` on any chat struct | "Grants are per workspace" needs a foundation that doesn't exist yet — phase 0, not a detail |
| No SQLite anywhere in the workspace | `grep -rn "rusqlite\|sqlite" --include=Cargo.toml .` → zero hits | One genuinely new dependency (decision below) |
| `GHOSTLINK_CTX_POLICY` / `GHOSTLINK_KEEP_LAST_TURNS` do not exist | grep returns nothing | The compaction hook must be built, not extended |

Reusable, already-correct: `resolve_workspace_path` (`main.rs:7290`) already
canonicalizes both sides and rejects `..`/symlink escapes — phase 2 scopes
tools with it rather than writing a second, weaker check.
`TaskRuntimeStore` (`task_runtime.rs:435`) already persists projects/tasks/reviews
as JSON and emits a `broadcast` event feed — phase 6 traces should append to
that rather than build a parallel bus.

---

## Decision: storage

**`rusqlite` with the `bundled` feature**, one DB file per workspace under the
data dir.

Chosen over a JSON store (mcp-rag's current pattern) because phases 1 *and* 5
both need durable local state and the brief commits to three things JSON makes
hard: ranked `memory_search` over growing bodies, cron scheduling that must
survive restart and query "what's due", and concurrent access from the chat
path, the scheduler task, and the approval tray without one global file lock.

`bundled` compiles SQLite from source — no system dep, no runtime installer, and
CI on all three platforms stays green. Cost is compile time on first build.

**This is the one reversible choice in the plan.** The store is behind a small
trait so a JSON backend can be swapped in without touching callers.

Secrets note: SQLite files inherit the data dir's permissions. The DB holds
memory bodies and tool previews — not credentials. API keys stay in
`api_keys.json` (hashed) and the OS keychain, per the security model.

---

## Implementation status

Branch: `feature/local-assistant-layer`. Phases 0 and 2 are landed and gated
(`cargo fmt`, `cargo clippy -D warnings`, `cargo test --workspace` all clean).

| Phase | Status |
|---|---|
| 0 — Workspace identity | **Done.** `workspace.rs`, `workspace_id` on `GuiChatRequest`, single shared `resolve_within` |
| 2 — Capability boundary | **Done.** `capability.rs`, enforced in `invoke_mcp_tool`, `GET /api/inference/capabilities` |
| 1 — Memory library | Next (needs the `rusqlite` decision) |
| 3 — Approval tray | Not started (needs persisted approvals; `CapabilityClass::parse` is its consumer) |
| 4 — Bounded loop | Not started |
| 5 — Scheduler | Not started |
| 6 — Turn traces | Partially: `chat` and `tool_confirm` audit events now carry the workspace binding and capability class |

Two notes for whoever picks this up:

- `invoke_mcp_tool` takes the workspace from `active_workspace()` rather than a
  new parameter. All six call sites (three engine loops plus tests) would
  otherwise need threading it, and the per-request binding is not yet consumed
  by anything downstream of the gate — the audit event is where it surfaces
  today. Revisit when memories/RAG actually resolve per workspace.
- The gate currently blocks with a `ToolResult` error rather than emitting a
  pending handle. That is deliberate for this pass: it makes the boundary
  enforceable and visible immediately without touching the streaming contract.
  Phase 3 replaces the block with the queued handle.

## Phase 0 — Workspace identity *(prerequisite, small)*

Everything in phases 1–5 is scoped per workspace. Today nothing carries a
workspace id, so this lands first or the scoping rules have nowhere to attach.

- Add `workspace_id: String` to the chat request struct and `ToolResult`.
- Derive it from `GHOSTLINK_WORKSPACE_ROOT`'s canonicalized path (short hash for
  the id string), defaulting to the existing single-workspace behavior so no
  current deployment changes shape.
- Thread it through the three engine paths alongside the existing `history`
  parameter — same plumbing pattern `native_tool_loop_core` already uses.
- Reject path escape with `resolve_workspace_path`; a chat bound to workspace A
  must not reach workspace B's files, RAG index, or memories.

Tests: two workspaces, assert cross-workspace reads are refused.

Files: `main.rs`, `mcp/registry.rs`. Docs: `SECURITY_MODEL.md` (trust boundary).

---

## Phase 1 — Memory / context library

New internal crate `crates/mcp-memory` (`publish = false`), matching
`crates/mcp-rag`'s stdio-MCP shape: single `main.rs`, `rmcp` + `tokio` +
`serde`, no framework.

Schema:

```sql
CREATE TABLE memories (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL,
  kind TEXT NOT NULL,      -- preference|project_fact|decision|person|open_loop|summary
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  source TEXT NOT NULL,    -- user|compaction|tool
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  pinned INTEGER NOT NULL DEFAULT 0,
  shared INTEGER NOT NULL DEFAULT 0   -- workspace-pinned collection
);
CREATE INDEX idx_mem_ws_kind ON memories(workspace_id, kind);
```

Tools: `memory_catalog` (titles/kinds/scopes only — never bodies),
`memory_search` (ranked hits, bodies only for returned hits), `memory_remember`
(write, approval-gated), `memory_forget` (write, approval-gated).

Ranking: FTS5 if it builds cleanly, else `LIKE` + recency/pinned boost. Decide
by actually compiling, not by assuming.

**Explicit-memory invariant:** the system prompt carries catalog titles + kinds
only. Bodies come from `memory_search` hits. Never stuff the store into the
prompt.

Register in `mcp_servers.example.toml` with a real `slot`, `enabled = true`.

Compaction hook: when the context policy drops turns, emit a compaction card
the user accepts/rejects in the GUI; on accept, write a `summary` memory with
`source = 'compaction'` and auto-approve that write only.

Tests: workspace isolation, pin/shared visibility, approval required on write.

---

## Phase 2 — Capability boundary in the MCP loop

The enforcement point. Classification lives in Rust and is checked *before*
dispatch; the system-prompt rule stays a hint, never the control.

- `crates/ghost-link/src/capability.rs`: `enum CapabilityClass { Read, Write, Exec }`.
- `fn classify(server: &str, tool: &str) -> CapabilityClass` — explicit table
  keyed on server+tool; **unknown tools default to `Exec`**.
- Annotate every tool in `mcp_servers.example.toml`'s active set. RAG
  `search`/`index_document` and memory reads → `Read`; memory writes → `Write`;
  terminal, code-execution, docker → `Exec`.
- Enforce at the three `call_tool` sites (`main.rs:2527`, `:2827`, plus the
  resume path):
  - `Read` → run, record an observation on the turn.
  - `Write`/`Exec` → enqueue an approval, return a pending handle to the model.
    Do not execute.
  - Vetted workspace-edit path may auto-apply **only** inside the workspace root
    (`resolve_workspace_path`), and still records the action.
- `approved_for_session` is tool+workspace scoped and **never** covers `Exec`.

Tests: unknown tool → Exec; `Exec` never auto-applies; write outside root is
refused even on the vetted path.

---

## Phase 3 — Approval tray (reshaped, per decision)

Reuses the existing `PendingToolCall` + resume endpoint — smallest diff, keeps
the SSE/JSON-line streaming contract untouched.

- Add `workspace_id`, `status`, `created_at`, and a `preview` (diff / SQL /
  command / URL) to the pending record.
- Statuses: `pending`, `approved`, `edited`, `denied`, `approved_for_session`.
- Persist pending actions to disk so an approval survives a restart.
- Add the non-blocking tray: write/exec return a pending handle the model can
  reason about, instead of stalling the whole turn the way the current flow does.
- GUI `ghostlink_gui_modern`: review tray on the chat with approve / edit /
  deny / approve-for-session. Entry point stays the control-plane gateway on
  `:8000` — never the Rust API port.

Files: `main.rs`, new `approvals.rs`, `ghostlink_gui_modern/src/components/ChatTab.tsx`.

Docs: `SECURITY_MODEL.md` — the trust model changes (see below).

---

## Phase 4 — Bounded agent loop

Replace single-shot tool calling with explicit plan → act → observe → stop.

- Refactor `native_tool_loop_core` into a named-step loop with budgets:
  max steps, max tool calls, max tokens. Existing `MAX_TOOL_ITERATIONS = 6`
  (`mcp/toolcall.rs:24`) becomes one of these budgets, keeping its documented
  rationale — the comment explaining the 3→6 raise stays intact.
- Stop on: final answer, awaiting approval, or budget exhausted.
- Turn environment exposes named bindings — `memory`, `rag`, `fs`, `scheduler` —
  discovered via catalog calls, not a giant static prompt.
- Skills are optional data: short procedure + the tools it may use, loaded on
  demand from `.agents/skills` or a SQLite table. **A skill cannot widen the
  capability boundary** — enforce by intersecting a skill's tool set with
  phase 2's classification before offering either.

Files: `mcp/toolcall.rs`, `main.rs`, `capability.rs`.

---

## Phase 5 — Scheduler

`ScheduleDriver`: SQLite rows + a tokio task.

Fields: `id`, `workspace_id`, `cron` or `run_at`, `prompt`, `enabled`,
`last_run`, `last_status`.

Each run is a normal agent turn with that workspace's capabilities. Read-only
schedules may auto-run; anything `write`/`exec` lands in the phase 3 queue.

GUI: list, enable, disable, run-now.

Note: cron parsing without a crate means implementing match semantics ourselves
— keep it to a documented 5-field subset and reject the rest at insert time.

---

## Phase 6 — Turn traces

Append structured events to the existing audit trail (`audit_log.rs`) and the
`TaskRuntimeStore` broadcast feed — no parallel bus.

Events: `invoke_agent`, `chat`, `execute_tool`, `tool_approval`. Include token
counts and latency.

**Never log** prompts, completions, file bodies, SQL, commands, or secrets.
Traces record tool name, class, decision, tokens, latency — nothing else.

GUI: render in the existing task/event view.

---

## Traceability

| Brief item | Phase | Status |
|---|---|---|
| Memory / context library | 1 | New crate + compaction hook |
| Capability boundary | 2 | New `capability.rs`; enforcement at 3 call sites |
| Approval tray | 3 | Reshape of existing flow (per your decision) |
| Bounded agent loop | 4 | Refactor of `native_tool_loop_core` |
| Scheduler | 5 | New `ScheduleDriver` |
| Turn traces | 6 | Append to existing audit trail |
| Workspace scoping (precondition) | 0 | **Not in the brief — required first** |

## Docs to update

- `SECURITY_MODEL.md` — the model is not the trust boundary; the server is.
  Capability classification, per-workspace grants, and approval scopes are new
  controls. Must be updated when phase 2 lands, not at the end.
- `CHANGELOG.md` — bold one-line summary, then file/function names and the *why*,
  per repo convention.
- `mcp_servers.example.toml` — capability annotations.
- `docs/ARCHITECTURE.md` — phase 0's workspace identity.

## Verification gates

Per `AGENTS.md`, before considering any phase done:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# GUI phases additionally:
cd ghostlink_gui_modern && npx tsc --noEmit && npx eslint . && npx vitest run
```

Acceptance: a single-node `launch.sh` path can chat, search memory, queue a
write, approve it, and run a read-only schedule without a second machine.

No benchmark numbers will be invented. If a phase needs a measurement that hasn't
been taken, it gets measured first — `docs/BENCHMARKS.md` records the noise
floor and the multi-run methodology.

## Suggested slicing

Each phase below compiles, tests, and can be reviewed on its own:

1. Phase 0 alone (workspace identity) — small, unblocks everything
2. Phase 1 slices: schema + `memory_catalog`/`memory_search` → writes +
   approval gate → compaction hook
3. Phase 2: classification table + tests → enforcement at call sites →
   vetted workspace-edit path
4. Phase 3: record fields + persistence → statuses → non-blocking handle →
   GUI tray
5. Phase 4 → 5 → 6

Phases 5 and 6 are independent of each other and can go in either order or in
parallel after 3. Phase 6 is the cheapest and can land early to make the rest
observable.