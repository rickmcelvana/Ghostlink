# Ghostlink Task Agents & Runtime (v2.3)

Ghostlink Studio includes a task agent runtime that enables users to create durable projects, spawn task agents, execute against workspace + MCP tools, and present human-in-the-loop `ReviewPacket` proposals before any mutations touch the live project root.

## Core Design Principles

1. **Contract**: A task is not done when the model stops talking. A task is done when a `ReviewPacket` exists and a human decides `accept`, `request_changes`, or `reject`.
2. **Staging Isolation**: Mutating tools write to a task staging area (`.ghostlink/tasks/<task_id>/proposed/`). The live project tree is untouched until `POST /api/reviews/:id/decide` with `decision: "accept"` applies the staged files through canonicalized workspace path verification.
3. **Reversible Applies**: Every `accept` snapshots the live files it is about to overwrite into `.ghostlink/tasks/<task_id>/backup/` (plus a manifest of which paths it created) *before* touching anything. `decision: "rollback"` restores the overwritten files and deletes the created ones. An accept is therefore undoable, and a failed apply is reported rather than silently marking the task `accepted`.
4. **Deterministic Safety**: Shell and mutation tools pass through a deterministic `Judge` policy (allowlist for search/lint/test, denylist for destructive/secret-exfiltrating commands, and pause for unrecognized commands requiring human approval).
5. **Self-Hosted Fabric**: The agent runtime consumes the existing Ghostlink cluster fabric, OpenAI-compatible `/v1/chat/completions`, and MCP tool registry.

## Objects & Data Model

Persisted under the Ghostlink data directory (`GHOSTLINK_DATA_DIR` or `.ghostlink/data`):

- **Project**: `id`, `name`, `kind` (`code` | `work`), `root_path`, `allowed_tools[]`, `default_model?`, `created_at`
- **Task**: `id`, `project_id`, `goal`, `acceptance_criteria?`, `status` (`queued` | `running` | `needs_review` | `accepted` | `rejected` | `blocked` | `cancelled`), `parent_id?`, `budget` (`max_steps`, `max_tokens?`, `max_minutes`), `created_at`, `updated_at`
- **AgentRun**: `id`, `task_id`, `role` (`implementer` | `planner`), `model`, `status`, `step_count`, `token_count`, `started_at`, `finished_at?`, `error?`
- **ReviewPacket**: `id`, `task_id`, `run_id`, `summary`, `diffs[]` (`path`, `unified_diff`, `original`, `proposed`), `commands[]` (`argv`, `judge`, `exit`, `excerpt`), `checks[]`, `risks[]`, `created_at`
- **Event**: `task_id`, `ts`, `kind`, `payload` (SSE stream)

## REST & SSE API

All routes are authenticated via Bearer token (API Key or JWT) or `?access_token=` query parameter (for SSE EventSource streams) and gated by role (`Viewer` read-only, `Operator`/`Owner` may spawn and decide):

```http
POST   /api/projects                            # Create project
GET    /api/projects                            # List projects
GET    /api/projects/:id                        # Get project details
PATCH  /api/projects/:id                        # Update project details

POST   /api/projects/:id/tasks                  # Create task
GET    /api/projects/:id/tasks                  # List tasks for project
GET    /api/tasks/:id                           # Get task details
GET    /api/tasks/:id/children                  # Get child tasks for parent task
POST   /api/tasks/:id/spawn                     # Spawn implementer agent run
POST   /api/tasks/:id/cancel                    # Cancel active run
GET    /api/tasks/:id/events                    # SSE event stream (text/event-stream)

GET    /api/tasks/:id/review                    # Get task ReviewPacket
POST   /api/reviews/:id/decide                  # Submit human decision (accept | request_changes | reject | rollback)
```

## Deterministic Judge Policy

Before running shell commands or file mutations, the runtime passes the request through a deterministic `Judge`:

- **ALLOW**: Read-only & inspection tools (`ls`, `rg`, `grep`, `git status`, `git diff`, `git log`) and safe test/lint runners (`cargo test`, `npx vitest run`, `npx tsc --noEmit`, `python -m pytest`).
- **DENY**: Destructive file commands (`rm -rf`, disk formatting), rewriting `.git` history (`git reset --hard`), secret exfiltration via `curl`/`wget`, docker container escape, and privilege escalation (`sudo`, `su`).
- **PAUSE**: Any unrecognized command pauses execution, sets task status to `blocked`, and emits a `judge` pause event pending human review.

## Implementation Status (v2.3 vs v2.4 Planned)

### Implemented in v2.4 Task Agents & Fan-out:
- **Task Runtime Engine (`crates/ghost-link/src/task_runtime.rs`)**: Store atomic JSON persistence under `GHOSTLINK_DATA_DIR`, canonical path isolation in `.ghostlink/tasks/<task_id>/proposed/`, `Judge` policy evaluation, `TaskRunner` implementer loop with budget constraints, and SSE broadcast channels.
- **Task API Server Handlers (`crates/ghost-link/src/task_api.rs`)**: Axum routes for `/api/projects`, `/api/tasks`, `/api/reviews`, and EventSource SSE event streaming with RBAC (`Viewer` read-only vs `Operator` mutation) and `?access_token=` authentication. Studio EventSource and control-plane now share jwt_secret.txt and accept ?access_token= on task SSE only.

### Implemented v2.4 Capabilities:
- **Studio Chat Agent Mode & GUI Wiring**: Agent Mode toggle in Studio Chat, workspace `root_path` validation, `POST /api/chat/agent` dispatch (or create project/task fallback), live task card SSE event streams, and inline `ReviewPane` decisions with `request_changes` respawning. Shared authenticated `GhostlinkAPI` client propagation across `ProjectsTab` and `TaskView` with base URL EventSource resolution and polling fallback.
- **Bounded Agent Tool Loop**: Real bounded tool-calling loop using in-process `AgentBackend` and OpenAI-compatible inference with deterministic `Judge` policy evaluation, staged file mutations in `.ghostlink/tasks/<id>/proposed/`, real shell tool execution, and budget controls (`max_steps`, `max_tokens`, `max_minutes`). Backend calls retry with backoff; token accounting uses the backend's reported usage when available.
- **v2.4 Child Fan-Out Trees**: Hierarchical child task creation (`parent_id`), budget inheritance, parent accept blocking (`check_parent_accept_allowed`), child task listing (`/api/tasks/:id/children`), and `planner` vs `implementer` role enforcement.
- **Reversible Accepts**: Pre-apply snapshots plus a `rollback` decision, so an accepted `ReviewPacket` can be undone.
- **Automatic Verification (definition of done)**: `VerificationPlan::detect` reads the project's own build files (`Cargo.toml`, a `package.json` `test` script, `pyproject.toml`/`pytest.ini`) and derives the commands that prove a change works — no configuration needed. Before a `ReviewPacket` is produced, the staged files are overlaid onto a scratch copy of the project and the plan is executed there; the live tree is never touched. Results land in `ReviewPacket.verification`, and `POST /api/reviews/:id/decide` **refuses `accept`** when verification failed or was never run, unless the caller passes `override_verification: true` (which is recorded in the response). Plan commands run through the same `Judge` policy as agent-issued commands, so a plan cannot smuggle in a denied command.
- Per-role model routing across heterogeneous cluster nodes.
- Per-task proposed/ staging directory isolation for parallel agents.

### Known gaps (not yet implemented)

These are real limitations of the current runtime, stated here rather than left
implied — the same standard the rest of this repo's docs hold to:

- **Conversation-window management is basic.** The agent loop now packs its
  transcript to fit a token budget before every backend call
  (`ContextGovernor`): the system prompt and task goal are pinned and the most
  recent turns are kept, with older turns dropped oldest-first and the
  compaction surfaced as a `risks[]` entry plus a `context_compacted` event.
  What compaction dropped is also carried in the packet's `context_summary`
  (turn count and the files touched, recovered from `write_file` tool-call
  arguments — not inferred, so it cannot name a file never written). What is
  still missing: an LLM-generated summary of the dropped turns, and a
  per-project budget — it uses `ContextGovernor::default()`.
- **Single-node by default.** With no cluster attached the runtime behaves
  exactly as before: no cluster prompt, no placement decision, no
  `placement_decided` event, and the `placement` / `accelerator_usage` /
  `context_summary` fields absent from the `ReviewPacket` rather than null.
- **Verification gets one automatic retry.** The plan runs once at the end of a
  run. If it failed and changes were proposed, the failure is fed back to the
  model for exactly one more attempt (`MAX_AUTO_FIX_ATTEMPTS = 1`), bounded by
  the remaining step budget — the retry consumes steps from the same total, so
  it cannot loop. If that still fails, the result is reported to the reviewer
  and blocks `accept`, which requires `request_changes` for anything further.
  Detection only covers Cargo / npm / pytest; other ecosystems need an explicit
  plan (not yet exposed via the API).
- **MCP Read tools are available on request.** A project listing `mcp_read` in
  `allowed_tools` gets the connected MCP servers' `Read`-class tools
  (`rag.search`, `memory.memory_search`, `git`-class reads, ...) as OpenAI-format
  function definitions. They are gated twice — by the project policy and by the
  capability class re-checked at dispatch — and Write/Exec tools are never
  exposed to the loop. Without `mcp_read` the toolset is the four built-ins
  (`read_file`, `write_file`, `run_command`, `spawn_subagent`) plus the MCP
  tools being absent entirely.
- **Git reads are allowed; commits are not.** Tasks are not isolated on a branch
  or worktree, and there is no commit step. The `Judge` allows
  `git status`/`diff`/`log`/`show`/`branch`/`stash`; `git diff` output reaches
  the model as unstructured text (a structured `git_diff_structured` tool is
  planned, not implemented).
- **One automatic fix cycle, then human review.** A run produces one
  `ReviewPacket`; beyond the single auto-fix attempt there is no further
  "run tests, read the failure, fix, re-run" cycle.
- **Fan-out depth is capped at 1** (`max_depth=1`, `max_children=4`).
