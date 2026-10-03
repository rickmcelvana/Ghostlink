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
- Per-role model routing across heterogeneous cluster nodes.
- Per-task proposed/ staging directory isolation for parallel agents.

### Known gaps (not yet implemented)

These are real limitations of the current runtime, stated here rather than left
implied — the same standard the rest of this repo's docs hold to:

- **No conversation-window management.** The agent's `messages` vector grows
  unbounded across steps; only individual tool *outputs* are truncated. On a
  small local context window a long task will hit a context overflow rather
  than compacting or summarizing. (`native_engine.rs`'s Context Governor is
  not wired into this loop.)
- **Checks are advisory, not enforced.** `ReviewPacket.checks` only contains
  commands the *model chose* to run. Nothing automatically runs a project's
  test suite after a mutation, and a packet with zero checks is still
  acceptable — so "it passed checks" means "the model ran something", not
  "the project's tests pass".
- **No retrieval or code intelligence.** The loop has four tools
  (`read_file`, `write_file`, `run_command`, `spawn_subagent`) and no symbol
  search, file outline, or embedding-based retrieval. The `mcp-rag` crate and
  the MCP tool registry are not bridged into the agent loop.
- **No git integration.** Tasks are not isolated on a branch or worktree, and
  there is no commit step; `git diff` is not available to the agent (the
  `Judge` allows only `git status`/`diff`/`log`, and its output is
  unstructured for the model).
- **Single-shot rather than iterative.** A run produces one `ReviewPacket`;
  there is no automatic "run tests, read the failure, fix, re-run" cycle
  beyond whatever the model does within its step budget.
- **Fan-out depth is capped at 1** (`max_depth=1`, `max_children=4`).
