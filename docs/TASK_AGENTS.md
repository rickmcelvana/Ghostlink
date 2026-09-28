# Ghostlink Task Agents & Runtime (v2.3)

Ghostlink Studio includes a task agent runtime that enables users to create durable projects, spawn task agents, execute against workspace + MCP tools, and present human-in-the-loop `ReviewPacket` proposals before any mutations touch the live project root.

## Core Design Principles

1. **Contract**: A task is not done when the model stops talking. A task is done when a `ReviewPacket` exists and a human decides `accept`, `request_changes`, or `reject`.
2. **Staging Isolation**: Mutating tools write to a task staging area (`.ghostlink/tasks/<task_id>/proposed/`). The live project tree is untouched until `POST /api/reviews/:id/decide` with `decision: "accept"` applies the staged files through canonicalized workspace path verification.
3. **Deterministic Safety**: Shell and mutation tools pass through a deterministic `Judge` policy (allowlist for search/lint/test, denylist for destructive/secret-exfiltrating commands, and pause for unrecognized commands requiring human approval).
4. **Self-Hosted Fabric**: The agent runtime consumes the existing Ghostlink cluster fabric, OpenAI-compatible `/v1/chat/completions`, and MCP tool registry.

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
POST   /api/reviews/:id/decide                  # Submit human decision (accept | request_changes | reject)
```

## Deterministic Judge Policy

Before running shell commands or file mutations, the runtime passes the request through a deterministic `Judge`:

- **ALLOW**: Read-only & inspection tools (`ls`, `rg`, `grep`, `git status`, `git diff`, `git log`) and safe test/lint runners (`cargo test`, `npx vitest run`, `npx tsc --noEmit`, `python -m pytest`).
- **DENY**: Destructive file commands (`rm -rf`, disk formatting), rewriting `.git` history (`git reset --hard`), secret exfiltration via `curl`/`wget`, docker container escape, and privilege escalation (`sudo`, `su`).
- **PAUSE**: Any unrecognized command pauses execution, sets task status to `blocked`, and emits a `judge` pause event pending human review.

## Implementation Status (v2.3 vs v2.4 Planned)

### Implemented in v2.4 Task Agents & Fan-out:
- **Task Runtime Engine (`crates/ghost-link/src/task_runtime.rs`)**: Store atomic JSON persistence under `GHOSTLINK_DATA_DIR`, canonical path isolation in `.ghostlink/tasks/<task_id>/proposed/`, `Judge` policy evaluation, `TaskRunner` implementer loop with budget constraints, and SSE broadcast channels.
- **Task API Server Handlers (`crates/ghost-link/src/task_api.rs`)**: Axum routes for `/api/projects`, `/api/tasks`, `/api/reviews`, and EventSource SSE event streaming with RBAC (`Viewer` read-only vs `Operator` mutation) and `?access_token=` authentication.

### Implemented v2.4 Capabilities:
- **Bounded Agent Tool Loop**: Real bounded tool-calling loop using in-process `AgentBackend` and OpenAI-compatible inference with deterministic `Judge` policy evaluation, staged file mutations in `.ghostlink/tasks/<id>/proposed/`, real shell tool execution, and budget controls (`max_steps`, `max_tokens`, `max_minutes`).
- **v2.4 Child Fan-Out Trees**: Hierarchical child task creation (`parent_id`), budget inheritance, parent accept blocking (`check_parent_accept_allowed`), child task listing (`/api/tasks/:id/children`), and `planner` vs `implementer` role enforcement.
- Per-role model routing across heterogeneous cluster nodes.
- Git worktree management for parallel agents.
- Agent skills, voice interfaces, or cloud model routing.
