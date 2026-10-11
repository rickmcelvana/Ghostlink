# Concrete Plan: Hardware-Aware & Agentic Task Agents

**Date:** 2026-10-10  
**Scope:** Ghostlink Task Agent system — 4 workstreams, incremental, reviewable  
**Preserves:** Staging isolation, Judge policy, ReviewPacket human gate

---

## Current State Summary

| Component | File | What exists | What's missing |
|---|---|---|---|
| Task Runtime | `crates/ghost-link/src/task_runtime.rs` | `TaskRunner::run_loop`, `ContextGovernor` (truncation), `VerificationPlan` (one-shot), `Judge`, `TaskRuntimeStore`, SSE broadcast | No hardware awareness, no auto-fix loop, no MCP bridge, no git tools, no summarization |
| Task API | `crates/ghost-link/src/task_api.rs` | Axum routes, SSE replay+tail, `RealAgentBackend`, `DecideReviewReq` | No placement in ReviewPacket, no `Last-Event-ID`, no Bearer auth on SSE |
| Agent Budget | `crates/ghost-link/src/agent.rs` | `Budget`, `BudgetTracker`, `StopReason` | Not wired to task runtime (separate system) |
| Capability | `crates/ghost-link/src/capability.rs` | `CapabilityClass` (Read/Write/Exec), `decide()`, session grants | Not used by task runtime (task runtime has its own `Judge`) |
| Context Window | `crates/ghost-link/src/context_window.rs` | `WindowPolicy`, `apply()`, `conservative_token_estimate` | Not used by task runtime (task runtime has its own `ContextGovernor`) |
| Cluster | `crates/ghostlink-core/src/cluster.rs` | `NodeMetrics` (VRAM, RAM, GPU name, compute capability, latency, throughput) | Not queried by task runtime |
| MCP | `crates/ghost-link/src/mcp/` | `registry.rs`, `client.rs`, `toolcall.rs` | Not bridged into task agent toolset |
| GUI | `ghostlink_gui_modern/src/components/` | `ReviewPane.tsx`, `TaskView.tsx`, `HealthPanel.tsx` | Polling-based, no event-driven updates |

---

## Workstream 1: Hardware-Aware Agent Planning

### 1.1 Expose cluster topology to the task runtime

**New file:** `crates/ghost-link/src/cluster_bridge.rs`

```rust
/// Snapshot of cluster node capabilities for agent planning.
/// Read-only view over ghostlink-core's cluster state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCapability {
    pub node_id: String,
    pub name: String,
    pub status: String,           // "active" | "degraded" | "failed"
    pub vram_gb: f32,
    pub system_memory_gb: f32,
    pub compute_capability: String, // "cuda" | "rocm" | "vulkan" | "metal" | "npu" | "cpu"
    pub gpu_name: Option<String>,
    pub available_vram_gb: f32,
    pub avg_latency_us: f32,
    pub streaming_layers: Option<(usize, usize)>, // (current, total)
}

/// Query the live cluster for active nodes.
/// Returns empty vec if cluster is unavailable (single-node mode).
pub async fn query_cluster_nodes() -> Vec<NodeCapability>;
```

**Wire into `TaskRuntimeStore`:** Add a `cluster_bridge: Option<Arc<dyn ClusterBridge>>` field. When `None`, the runtime operates in single-node mode (current behavior).

### 1.2 Placement-aware model selection

**Modify:** `crates/ghost-link/src/task_api.rs` — `handle_spawn_task`

Before spawning, if cluster nodes are available, inject a system-prompt prefix describing available nodes and a placement hint:

```
Cluster topology for this run:
- node-a: CUDA, 24GB VRAM, RTX 4090, 12GB free
- node-b: CPU, 64GB RAM, no accelerator
- node-c: NPU, 16GB, Intel NPU

Placement guidance: heavy coding tasks → GPU nodes; light verification → CPU; vision → NPU if available.
```

The model then chooses `run_command` with `GHOSTLINK_NODE=node-a` env var or the planner spawns child tasks with a `preferred_node` field.

### 1.3 Surface placement in ReviewPacket and SSE

**Modify:** `crates/ghost-link/src/task_runtime.rs` — `ReviewPacket`

Add fields:
```rust
#[serde(default)]
pub placement: Option<PlacementDecision>,
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacementDecision {
    pub task_id: String,
    pub assigned_node: String,
    pub node_capability: NodeCapability,
    pub rationale: String, // why this node was chosen
    pub alternative_nodes: Vec<String>, // other candidates considered
}
```

Emit `placement_decided` SSE event when assigned.

### 1.4 Acceptance criteria for Workstream 1

- [ ] `GET /api/tasks/:id` response includes `placement` when cluster is available
- [ ] SSE stream emits `placement_decided` event
- [ ] Single-node mode (no cluster) works identically to today
- [ ] `cargo test --workspace` passes

---

## Workstream 2: Stronger Agentic Loop

### 2.1 Auto-fix loop for verification failures

**Modify:** `crates/ghost-link/src/task_runtime.rs` — `TaskRunner::run_loop`

After the verification block (currently lines ~2076-2164), add:

```rust
// ---- Auto-Fix Loop ----------------------------------------------------
// If verification failed and we have budget remaining, feed the failure
// back to the model and let it try once more before giving up.
let mut auto_fix_attempts = 0;
const MAX_AUTO_FIX_ATTEMPTS: u32 = 1;

if let Some(plan) = &plan {
    if !diffs.is_empty() && verification.iter().any(|r| !r.passed) {
        if run.step_count < max_steps && auto_fix_attempts < MAX_AUTO_FIX_ATTEMPTS {
            auto_fix_attempts += 1;
            
            let failure_summary: String = verification
                .iter()
                .filter(|r| !r.passed)
                .map(|r| r.summary())
                .collect::<Vec<_>>()
                .join("\n");
            
            store.emit_event(TaskEvent {
                task_id: task.id.clone(),
                ts: Utc::now().timestamp_millis() as u64,
                kind: "auto_fix_triggered".into(),
                payload: serde_json::json!({
                    "attempt": auto_fix_attempts,
                    "max_attempts": MAX_AUTO_FIX_ATTEMPTS,
                    "failures": failure_summary,
                }),
            });
            
            // Inject verification failure as a user message and continue the loop
            messages.push(serde_json::json!({
                "role": "user",
                "content": format!(
                    "Verification failed:\n{}\n\nFix the proposed files so the verification passes. \
                     Update the files in the proposed/ directory.",
                    failure_summary
                ),
            }));
            
            // Continue the loop (don't break) — the model gets another chance
            // The loop's step budget still applies, so this can't run forever.
            continue;
        }
    }
}
```

**Key constraint:** The `continue` re-enters the `for _step in 1..=max_steps` loop. Since `run.step_count` was already incremented, the loop will exhaust after `max_steps` total iterations. The auto-fix attempt consumes one of those steps. This is bounded by design.

### 2.2 Bridge MCP tools into the task agent toolset

**New file:** `crates/ghost-link/src/mcp_bridge.rs`

```rust
/// Adapts MCP tool calls into the task agent's ToolCall format.
/// Only Read-class tools are exposed to implementer agents.
/// Write/Exec tools require human approval via the existing capability system.
pub fn list_mcp_tools_for_agent(
    registry: &McpRegistry,
    allowed_classes: &[CapabilityClass], // default: [Read]
) -> Vec<Value>; // OpenAI-format tool definitions

/// Execute an MCP tool call and return the result as a string.
/// Returns Err for Write/Exec tools that lack approval.
pub async fn execute_mcp_tool(
    registry: &McpRegistry,
    server: &str,
    tool: &str,
    args: &Value,
    workspace_id: &str,
    workspace_root: &Path,
) -> Result<String>;
```

**Modify:** `crates/ghost-link/src/task_api.rs` — `build_tool_definitions`

Add MCP Read tools (e.g., `rag.search`, `filesystem.search_files`, `git.git_diff`) to the tool list when the project's `allowed_tools` includes `"mcp_read"`.

**Modify:** `crates/ghost-link/src/task_runtime.rs` — tool dispatch in `run_loop`

Add new match arms for MCP tools:
```rust
"mcp_search" | "mcp_git_diff" | "mcp_file_search" => {
    // Delegate to mcp_bridge
    let result = mcp_bridge::execute_mcp_tool(...).await;
    messages.push(/* result */);
}
```

### 2.3 Structured git awareness

**Modify:** `crates/ghost-link/src/task_runtime.rs` — `Judge::is_allowed`

Expand the git allowlist:
```rust
"git" => {
    if argv.len() >= 2 {
        matches!(argv[1].as_str(), 
            "status" | "diff" | "log" | "show" | "branch" | "stash" | "stash pop")
    } else {
        false
    }
}
```

**New tool:** `git_diff_structured` — returns a JSON summary of changed files, insertions, deletions, and per-file diffs (not raw `git diff` output). This gives the model a structured view it can reason about.

```rust
#[derive(Serialize, Deserialize)]
pub struct StructuredDiff {
    pub changed_files: Vec<FileDiff>,
    pub total_insertions: u32,
    pub total_deletions: u32,
    pub branch: String,
    pub ahead: i32,  // commits ahead of upstream
    pub behind: i32,
}

#[derive(Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub status: String, // "modified" | "added" | "deleted" | "renamed"
    pub insertions: u32,
    pub deletions: u32,
    pub diff: String, // unified diff for this file
}
```

### 2.4 Smarter context management

**Modify:** `crates/ghost-link/src/task_runtime.rs` — replace `ContextGovernor` usage

Replace the current truncation-only `ContextGovernor` with a two-phase approach:

1. **Selective retention** (existing): Keep system prompt, task goal, recent turns
2. **Lightweight summarization** (new): When compaction triggers, instead of dropping old turns, summarize them into a "memory blob" message:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSummary {
    pub original_turn_count: usize,
    pub summary_text: String,  // LLM-generated or heuristic summary
    pub key_decisions: Vec<String>,  // extracted action items
    pub files_modified: Vec<String>, // files touched in dropped turns
}
```

The summarization uses the same `AgentBackend` to call the model with a compaction prompt:
```
Summarize the following conversation turns into a brief memory blob.
Extract: (1) key decisions made, (2) files modified, (3) commands run and their outcomes.
Keep it under 200 tokens.
```

This is a single extra model call per compaction event, not per turn. The `context_compacted` SSE event now includes the summary.

### 2.5 Acceptance criteria for Workstream 2

- [ ] A failing verification triggers at least one automatic fix attempt (test with `FakeBackend`)
- [ ] MCP Read tools appear in tool list when `allowed_tools` contains `"mcp_read"`
- [ ] `git branch`, `git stash` pass the Judge
- [ ] Context compaction produces a summary blob, not just dropped turns
- [ ] `cargo test --workspace` passes

---

## Workstream 3: Event-Driven Agentic UX

### 3.1 Harden SSE

**Modify:** `crates/ghost-link/src/task_api.rs` — `handle_get_task_events`

Add:
1. **`Last-Event-ID` resume:** Parse the `Last-Event-ID` header. If present, replay events after that ID from the JSONL file instead of from the beginning.
2. **Bearer auth:** Currently uses `?access_token=`. Add `Authorization: Bearer <token>` header support.
3. **Bounded history:** Add a `?limit=N` query param (default 100) to cap replayed events.
4. **Connection status:** Emit a `connection_status` event every 30s (heartbeat) so the GUI can detect a dead connection.

```rust
async fn handle_get_task_events(
    State(state): State<Arc<Mutex<BackendState>>>,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
    Query(params): Query<SseParams>,
) -> impl IntoResponse {
    // Extract Last-Event-ID from headers
    let last_event_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok());
    
    // Authenticate via Bearer or ?access_token=
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .or_else(|| params.access_token.as_deref());
    
    // ... validate token ...
    
    // Replay from last_event_id if provided
    let historical_events = store.get_task_events_since(&id, last_event_id);
    // ... rest of streaming logic ...
}
```

### 3.2 Drive ReviewPane from events

**Modify:** `ghostlink_gui_modern/src/components/ReviewPane.tsx`

Replace polling with SSE-driven updates:

```typescript
useEffect(() => {
    const evtSource = new EventSource(
        `${API_BASE}/api/tasks/${taskId}/events?access_token=${token}`
    );
    
    evtSource.addEventListener('review_ready', (e) => {
        const review = JSON.parse(e.data);
        setReview(review);
        setVerificationStatus(review.verification_passed);
    });
    
    evtSource.addEventListener('auto_fix_triggered', (e) => {
        const data = JSON.parse(e.data);
        setAutoFixStatus({ attempt: data.attempt, max: data.max_attempts });
    });
    
    evtSource.addEventListener('placement_decided', (e) => {
        const data = JSON.parse(e.data);
        setPlacement(data);
    });
    
    evtSource.addEventListener('connection_status', (e) => {
        setConnected(true);
    });
    
    return () => evtSource.close();
}, [taskId, token]);
```

### 3.3 TaskView live updates

**Modify:** `ghostlink_gui_modern/src/components/TaskView.tsx`

- Subscribe to SSE for `step`, `tool`, `file`, `judge`, `verification_started`, `review_ready`, `error` events
- Update the task card in real time (step count, current tool, file changes)
- Show a live "agent is working..." indicator with the current action
- Show verification results as they arrive (pass/fail per command)

### 3.4 Reliable human decision surface

**Modify:** `ghostlink_gui_modern/src/components/ReviewPane.tsx`

- Disable Accept button when `verification_passed === false` (show override checkbox)
- Show `auto_fix_triggered` status ("Verification failed — auto-fix attempt 1/1 in progress...")
- Show `placement_decided` info ("Assigned to node-a: CUDA, 24GB VRAM")
- Add a "Request Changes" text input for the `note` field
- Show `connection_status` indicator (green/red dot)

### 3.5 Acceptance criteria for Workstream 3

- [ ] SSE resumes from `Last-Event-ID` (test: connect, disconnect, reconnect with last ID)
- [ ] `Authorization: Bearer <token>` works on `/api/tasks/:id/events`
- [ ] ReviewPane updates without polling when `review_ready` fires
- [ ] TaskView shows live step count and current tool
- [ ] Accept button is disabled when verification fails (unless override checked)
- [ ] `cargo test --workspace` + `npx vitest run` pass

---

## Workstream 4: Observability for Heterogeneous Runs

### 4.1 Emit per-node execution events

**Modify:** `crates/ghost-link/src/task_runtime.rs` — tool dispatch

When a `run_command` executes, include the target node in the event:

```rust
store.emit_event(TaskEvent {
    task_id: task.id.clone(),
    ts: Utc::now().timestamp_millis() as u64,
    kind: "tool".into(),
    payload: serde_json::json!({
        "name": tool_call.name,
        "args": tool_call.args,
        "node": node_capability.node_id,  // NEW
        "node_compute": node_capability.compute_capability,  // NEW
    }),
});
```

### 4.2 Accelerator usage in ReviewPacket

**Modify:** `crates/ghost-link/src/task_runtime.rs` — `ReviewPacket`

Add:
```rust
#[serde(default)]
pub accelerator_usage: Option<AcceleratorUsage>,
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceleratorUsage {
    pub node_id: String,
    pub compute_capability: String,
    pub gpu_name: Option<String>,
    pub vram_used_gb: f32,
    pub commands_executed: u32,
    pub tokens_generated: u32,
}
```

### 4.3 Cross-node traffic metrics

**Modify:** `crates/ghost-link/src/task_runtime.rs` — `ReviewPacket`

Add:
```rust
#[serde(default)]
pub cross_node_traffic: Option<CrossNodeTraffic>,
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossNodeTraffic {
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub nodes_contacted: Vec<String>,
    pub model_transfers: Vec<ModelTransfer>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTransfer {
    pub from_node: String,
    pub to_node: String,
    pub model_name: String,
    pub bytes: u64,
}
```

### 4.4 Acceptance criteria for Workstream 4

- [ ] `tool` SSE events include `node` and `node_compute` fields
- [ ] ReviewPacket includes `accelerator_usage` when run on a cluster node
- [ ] ReviewPacket includes `cross_node_traffic` when models were transferred
- [ ] `cargo test --workspace` passes

---

## Implementation Order & Increments

### Increment 1: Foundation (Workstream 1.1 + 1.2)
- Create `cluster_bridge.rs`
- Wire into `TaskRuntimeStore`
- Inject cluster topology into system prompt
- **Verify:** Run a task on a multi-node cluster, see topology in system prompt

### Increment 2: Placement (Workstream 1.3 + 4.1)
- Add `PlacementDecision` to `ReviewPacket`
- Emit `placement_decided` event
- Add node info to `tool` events
- **Verify:** `GET /api/tasks/:id` shows placement; SSE shows placement event

### Increment 3: Auto-fix loop (Workstream 2.1)
- Add auto-fix logic to `run_loop`
- Emit `auto_fix_triggered` event
- **Verify:** Write a test with `FakeBackend` that fails verification then fixes it

### Increment 4: MCP bridge (Workstream 2.2)
- Create `mcp_bridge.rs`
- Add MCP tools to `build_tool_definitions`
- Add MCP tool dispatch to `run_loop`
- **Verify:** `mcp_search` tool appears in tool list and returns results

### Increment 5: Git awareness (Workstream 2.3)
- Expand Judge git allowlist
- Add `git_diff_structured` tool
- **Verify:** `git branch` passes Judge; structured diff returns JSON

### Increment 6: Context summarization (Workstream 2.4)
- Add `ContextSummary` type
- Implement summarization call in compaction path
- **Verify:** Compaction produces a summary blob, not just dropped turns

### Increment 7: SSE hardening (Workstream 3.1)
- Add `Last-Event-ID`, Bearer auth, bounded history, heartbeat
- **Verify:** Reconnect with `Last-Event-ID` resumes correctly

### Increment 8: Event-driven GUI (Workstream 3.2-3.4)
- Update `ReviewPane.tsx` to use SSE
- Update `TaskView.tsx` to use SSE
- Add connection status indicator
- **Verify:** GUI updates without polling; Accept disabled on verification failure

### Increment 9: Observability (Workstream 4.2-4.4)
- Add `AcceleratorUsage` and `CrossNodeTraffic` to `ReviewPacket`
- **Verify:** ReviewPacket shows accelerator usage and traffic metrics

### Increment 10: Documentation
- Update `docs/TASK_AGENTS.md` with new capabilities
- Update `docs/API_REFERENCE.md` with new fields and events
- Update `docs/ARCHITECTURE.md` with cluster bridge and MCP bridge

---

## Risk Mitigation

| Risk | Mitigation |
|---|---|
| Auto-fix loop consumes entire step budget | Bounded by `max_steps`; auto-fix is one iteration, not a nested loop |
| MCP tools expose dangerous operations | Only `CapabilityClass::Read` tools are exposed; Write/Exec still gated |
| Context summarization adds latency | Only triggers on compaction (rare); uses same backend, no new dependency |
| SSE `Last-Event-ID` replay is expensive | Bounded by `?limit=N` (default 100); JSONL is append-only, seek is O(1) |
| Cluster bridge fails | `Option<Arc<dyn ClusterBridge>>` — `None` means single-node, no behavior change |
| GUI SSE reconnection storms | Exponential backoff in EventSource; heartbeat detects dead connections |

---

## Files Touched Summary

| File | Change type | Workstream |
|---|---|---|
| `crates/ghost-link/src/cluster_bridge.rs` | **NEW** | 1 |
| `crates/ghost-link/src/task_runtime.rs` | Modify | 1, 2, 4 |
| `crates/ghost-link/src/task_api.rs` | Modify | 1, 2, 3 |
| `crates/ghost-link/src/mcp_bridge.rs` | **NEW** | 2 |
| `crates/ghost-link/src/agent.rs` | Modify (wire Budget into task runtime) | 2 |
| `crates/ghost-link/src/context_window.rs` | Modify (use in task runtime) | 2 |
| `ghostlink_gui_modern/src/components/ReviewPane.tsx` | Modify | 3 |
| `ghostlink_gui_modern/src/components/TaskView.tsx` | Modify | 3 |
| `docs/TASK_AGENTS.md` | Modify | 10 |
| `docs/API_REFERENCE.md` | Modify | 10 |
| `docs/ARCHITECTURE.md` | Modify | 10 |

---

## What NOT changed

- **Staging isolation:** All writes still go to `.ghostlink/tasks/<task_id>/proposed/`
- **Judge policy:** Still the single authority on command execution; auto-fix commands pass through it
- **ReviewPacket gate:** Human still must accept/reject/rollback; auto-fix just gives the model one more chance before the packet is produced
- **Verification gate:** `override_verification` still required to accept a failing change
- **Fan-out caps:** `max_depth=1`, `max_children=4` unchanged
- **Budget model:** `max_steps`, `max_tokens`, `max_minutes` unchanged
