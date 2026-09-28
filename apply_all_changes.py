import os, sys, re

# 1. Update task_runtime.rs with Slice 1 & 2
with open("crates/ghost-link/src/task_runtime.rs", "r") as f:
    tr = f.read()

# Add AgentBackend trait & ToolCall / AgentResponse
trait_code = """
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

#[async_trait::async_trait]
pub trait AgentBackend: Send + Sync {
    async fn chat(
        &self,
        messages: &[serde_json::Value],
        allowed_tools: &[String],
    ) -> Result<AgentResponse>;
}

// Helper to recursively list relative paths in a directory
fn list_dir_relative(base_dir: &Path, current_dir: &Path, rel_paths: &mut Vec<String>) -> Result<()> {
    if !current_dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(current_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            list_dir_relative(base_dir, &path, rel_paths)?;
        } else if path.is_file() {
            if let Ok(rel) = path.strip_prefix(base_dir) {
                rel_paths.push(rel.to_string_lossy().to_string());
            }
        }
    }
    Ok(())
}
"""

list_child_tasks_code = """    pub fn list_child_tasks(&self, parent_id: &str) -> Result<Vec<Task>> {
        let parent = self.get_task(parent_id)?;
        let all_tasks = self.list_project_tasks(&parent.project_id)?;
        Ok(all_tasks.into_iter().filter(|t| t.parent_id.as_deref() == Some(parent_id)).collect())
    }
"""

runner_code = """// Implementer Loop Runner
pub struct TaskRunner;

impl TaskRunner {
    pub async fn spawn_implementer(
        store: Arc<TaskRuntimeStore>,
        backend: Arc<dyn AgentBackend>,
        task: Task,
        role: String,
        model: String,
        brief: Option<String>,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<AgentRun> {
        let now = Utc::now().to_rfc3339();
        let run = AgentRun {
            id: format!("run_{}", Uuid::new_v4().simple()),
            task_id: task.id.clone(),
            role,
            model,
            status: "running".into(),
            step_count: 0,
            token_count: 0,
            started_at: now,
            finished_at: None,
            error: None,
        };

        store.save_run(&run)?;
        store.update_task_status(&task.id, TaskStatus::Running)?;
        store.register_cancel_token(task.id.clone(), cancel_token.clone());

        let store_clone = Arc::clone(&store);
        let task_clone = task.clone();
        let mut run_clone = run.clone();

        tokio::spawn(async move {
            let res = Self::run_loop(
                store_clone.clone(),
                backend,
                task_clone.clone(),
                &mut run_clone,
                brief,
                cancel_token,
            )
            .await;

            let finished_at = Utc::now().to_rfc3339();
            run_clone.finished_at = Some(finished_at.clone());

            match res {
                Ok((review, final_status)) => {
                    run_clone.status = "finished".into();
                    let _ = store_clone.save_run(&run_clone);
                    let _ = store_clone.save_review(&review);
                    let _ = store_clone.update_task_status(&task_clone.id, final_status);

                    store_clone.emit_event(TaskEvent {
                        task_id: task_clone.id.clone(),
                        ts: Utc::now().timestamp_millis() as u64,
                        kind: "review_ready".into(),
                        payload: serde_json::to_value(&review).unwrap_or_default(),
                    });
                }
                Err(err) => {
                    let err_msg = err.to_string();
                    let is_cancelled = err_msg.contains("cancelled");
                    run_clone.status = if is_cancelled {
                        "cancelled".into()
                    } else {
                        "failed".into()
                    };
                    run_clone.error = Some(err_msg.clone());
                    let _ = store_clone.save_run(&run_clone);

                    let status = if is_cancelled {
                        TaskStatus::Cancelled
                    } else {
                        TaskStatus::Blocked
                    };
                    let _ = store_clone.update_task_status(&task_clone.id, status);

                    store_clone.emit_event(TaskEvent {
                        task_id: task_clone.id.clone(),
                        ts: Utc::now().timestamp_millis() as u64,
                        kind: if is_cancelled {
                            "cancelled".into()
                        } else {
                            "error".into()
                        },
                        payload: serde_json::json!({ "error": err_msg }),
                    });
                }
            }
        });

        Ok(run)
    }

    async fn run_loop(
        store: Arc<TaskRuntimeStore>,
        backend: Arc<dyn AgentBackend>,
        task: Task,
        run: &mut AgentRun,
        brief: Option<String>,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<(ReviewPacket, TaskStatus)> {
        let project = store.get_project(&task.project_id)?;

        store.emit_event(TaskEvent {
            task_id: task.id.clone(),
            ts: Utc::now().timestamp_millis() as u64,
            kind: "run_started".into(),
            payload: serde_json::json!({ "run_id": run.id, "brief": brief }),
        });

        let mut commands_executed = Vec::new();
        let mut checks = Vec::new();
        let mut risks = Vec::new();
        let mut final_status = TaskStatus::NeedsReview;

        let start_time = std::time::Instant::now();

        // Build initial messages
        let system_prompt = format!(
            "You are an AI task agent implementing project '{}' at root path '{}'. Use available tools to write proposed files and execute checks.",
            project.name, project.root_path
        );

        let mut user_prompt = format!("Task Goal: {}\\n", task.goal);
        if let Some(b) = &brief {
            user_prompt.push_str(&format!("Execution Brief: {}\\n", b));
        }
        if let Some(ac) = &task.acceptance_criteria {
            user_prompt.push_str(&format!("Acceptance Criteria: {}\\n", ac));
        }

        let mut messages = vec![
            serde_json::json!({ "role": "system", "content": system_prompt }),
            serde_json::json!({ "role": "user", "content": user_prompt }),
        ];

        let max_steps = task.budget.max_steps;
        let max_minutes = task.budget.max_minutes as u64;

        for _step in 1..=max_steps {
            if cancel_token.is_cancelled() {
                return Err(anyhow!("Task execution cancelled by user"));
            }

            if start_time.elapsed().as_secs() > max_minutes * 60 {
                risks.push("Exceeded time budget (max_minutes)".into());
                break;
            }

            if let Some(max_tok) = task.budget.max_tokens {
                if run.token_count >= max_tok {
                    risks.push("Exceeded token budget (max_tokens)".into());
                    break;
                }
            }

            run.step_count += 1;
            store.emit_event(TaskEvent {
                task_id: task.id.clone(),
                ts: Utc::now().timestamp_millis() as u64,
                kind: "step".into(),
                payload: serde_json::json!({ "step": run.step_count, "description": format!("Agent step {}", run.step_count) }),
            });

            // Call model via AgentBackend
            let agent_resp = match backend.chat(&messages, &project.allowed_tools).await {
                Ok(resp) => resp,
                Err(err) => {
                    risks.push(format!("Backend inference error: {}", err));
                    break;
                }
            };

            // Estimate tokens
            let content_str = agent_resp.content.clone().unwrap_or_default();
            run.token_count += (content_str.len() / 4).max(10) as u32;

            if !content_str.is_empty() {
                messages.push(serde_json::json!({ "role": "assistant", "content": content_str }));
            }

            if agent_resp.tool_calls.is_empty() {
                break;
            }

            let mut should_stop = false;

            for tool_call in agent_resp.tool_calls {
                store.emit_event(TaskEvent {
                    task_id: task.id.clone(),
                    ts: Utc::now().timestamp_millis() as u64,
                    kind: "tool".into(),
                    payload: serde_json::json!({ "name": tool_call.name, "args": tool_call.args }),
                });

                match tool_call.name.as_str() {
                    "spawn_subagent" | "create_child_task" => {
                        if run.role != "planner" {
                            risks.push("Implementer agent tried to spawn subagent (denied)".into());
                            messages.push(serde_json::json!({ "role": "user", "content": "Error: implementer agents cannot spawn subagents" }));
                            continue;
                        }

                        let child_goal = tool_call.args.get("goal").and_then(|g| g.as_str()).unwrap_or("");
                        let child_criteria = tool_call.args.get("acceptance_criteria").and_then(|c| c.as_str());

                        if child_goal.is_empty() {
                            messages.push(serde_json::json!({ "role": "user", "content": "Error: goal cannot be empty for child task" }));
                            continue;
                        }

                        let elapsed_mins = (start_time.elapsed().as_secs() / 60) as u32;
                        let rem_mins = task.budget.max_minutes.saturating_sub(elapsed_mins).max(1);
                        let rem_steps = task.budget.max_steps.saturating_sub(run.step_count).max(1);
                        let rem_tokens = task.budget.max_tokens.map(|mt| mt.saturating_sub(run.token_count));

                        let child_budget = TaskBudget {
                            max_steps: rem_steps,
                            max_tokens: rem_tokens,
                            max_minutes: rem_mins,
                        };

                        match store.create_task(&project.id, child_goal.to_string(), child_criteria.map(|s| s.to_string()), Some(child_budget), Some(task.id.clone())) {
                            Ok(child) => {
                                store.emit_event(TaskEvent {
                                    task_id: task.id.clone(),
                                    ts: Utc::now().timestamp_millis() as u64,
                                    kind: "child_created".into(),
                                    payload: serde_json::json!({ "child_id": child.id, "goal": child.goal }),
                                });
                                messages.push(serde_json::json!({ "role": "user", "content": format!("Successfully created child task #{} ({})", child.id, child.goal) }));
                            }
                            Err(e) => {
                                risks.push(format!("Failed to create child task: {}", e));
                                messages.push(serde_json::json!({ "role": "user", "content": format!("Failed to create child task: {}", e) }));
                            }
                        }
                    }
                    "write_file" => {
                        let rel_path = tool_call.args.get("path").and_then(|p| p.as_str()).unwrap_or("");
                        let file_content = tool_call.args.get("content").and_then(|c| c.as_str()).unwrap_or("");

                        if rel_path.is_empty() {
                            risks.push("write_file tool called with empty path".into());
                            messages.push(serde_json::json!({ "role": "user", "content": "Error: write_file path is empty" }));
                            continue;
                        }

                        if !project.allowed_tools.contains(&"write_file".to_string()) {
                            risks.push("write_file tool not allowed by project policy".into());
                            messages.push(serde_json::json!({ "role": "user", "content": "Error: write_file tool is disabled for this project" }));
                            continue;
                        }

                        match store.write_proposed_file(&project.root_path, &task.id, rel_path, file_content) {
                            Ok(_) => {
                                store.emit_event(TaskEvent {
                                    task_id: task.id.clone(),
                                    ts: Utc::now().timestamp_millis() as u64,
                                    kind: "file".into(),
                                    payload: serde_json::json!({ "path": rel_path, "action": "write_proposed" }),
                                });
                                messages.push(serde_json::json!({ "role": "user", "content": format!("Successfully wrote proposed file {}", rel_path) }));
                            }
                            Err(e) => {
                                risks.push(format!("Failed writing proposed file {}: {}", rel_path, e));
                                messages.push(serde_json::json!({ "role": "user", "content": format!("Error writing proposed file {}: {}", rel_path, e) }));
                            }
                        }
                    }
                    "read_file" => {
                        let rel_path = tool_call.args.get("path").and_then(|p| p.as_str()).unwrap_or("");
                        let proposed_file = store.get_task_proposed_dir(&project.root_path, &task.id).join(rel_path);
                        let file_text = if proposed_file.exists() {
                            fs::read_to_string(&proposed_file).unwrap_or_default()
                        } else if let Ok(live_p) = TaskRuntimeStore::validate_and_resolve_path(&project.root_path, rel_path) {
                            fs::read_to_string(&live_p).unwrap_or_default()
                        } else {
                            "File not found".to_string()
                        };
                        messages.push(serde_json::json!({ "role": "user", "content": format!("File content of {}:\\n{}", rel_path, file_text) }));
                    }
                    "run_command" | "exec" | "execute" | "shell" => {
                        let argv: Vec<String> = if let Some(arr) = tool_call.args.get("argv").and_then(|a| a.as_array()) {
                            arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                        } else if let Some(cmd_str) = tool_call.args.get("command").and_then(|c| c.as_str()) {
                            cmd_str.split_whitespace().map(|s| s.to_string()).collect()
                        } else {
                            vec![]
                        };

                        if argv.is_empty() {
                            messages.push(serde_json::json!({ "role": "user", "content": "Error: empty command" }));
                            continue;
                        }

                        let judge_eval = Judge::evaluate(&argv);
                        store.emit_event(TaskEvent {
                            task_id: task.id.clone(),
                            ts: Utc::now().timestamp_millis() as u64,
                            kind: "judge".into(),
                            payload: serde_json::json!({ "argv": argv, "decision": format!("{:?}", judge_eval) }),
                        });

                        match judge_eval {
                            JudgeResult::Allow => {
                                if !project.allowed_tools.contains(&"run_command".to_string()) && !project.allowed_tools.contains(&"execute".to_string()) {
                                    risks.push("Command execution tool not allowed by project policy".into());
                                    messages.push(serde_json::json!({ "role": "user", "content": "Error: command execution is disabled for this project" }));
                                    continue;
                                }

                                let output_res = tokio::process::Command::new(&argv[0])
                                    .args(&argv[1..])
                                    .current_dir(&project.root_path)
                                    .output()
                                    .await;

                                match output_res {
                                    Ok(output) => {
                                        let exit_code = output.status.code().unwrap_or(-1);
                                        let stdout = String::from_utf8_lossy(&output.stdout);
                                        let stderr = String::from_utf8_lossy(&output.stderr);
                                        let excerpt = format!("STDOUT:\\n{}\\nSTDERR:\\n{}", stdout, stderr);
                                        let trimmed_excerpt = if excerpt.len() > 1000 {
                                            format!("{}...\\n(truncated)", &excerpt[..1000])
                                        } else {
                                            excerpt
                                        };

                                        commands_executed.push(ReviewCommand {
                                            argv: argv.clone(),
                                            judge: JudgeDecision::Allow,
                                            exit: exit_code,
                                            excerpt: trimmed_excerpt.clone(),
                                        });

                                        if exit_code == 0 {
                                            checks.push(format!("Passed: {}", argv.join(" ")));
                                        } else {
                                            risks.push(format!("Command failed (exit {}): {}", exit_code, argv.join(" ")));
                                        }

                                        messages.push(serde_json::json!({
                                            "role": "user",
                                            "content": format!("Command '{}' exited with {}:\\n{}", argv.join(" "), exit_code, trimmed_excerpt)
                                        }));
                                    }
                                    Err(e) => {
                                        commands_executed.push(ReviewCommand {
                                            argv: argv.clone(),
                                            judge: JudgeDecision::Allow,
                                            exit: -1,
                                            excerpt: format!("Failed to spawn command: {}", e),
                                        });
                                        risks.push(format!(
                                            "Failed to execute command {}: {}",
                                            argv.join(" "),
                                            e
                                        ));
                                        messages.push(serde_json::json!({ "role": "user", "content": format!("Failed to execute command: {}", e) }));
                                    }
                                }
                            }
                            JudgeResult::Deny => {
                                commands_executed.push(ReviewCommand {
                                    argv: argv.clone(),
                                    judge: JudgeDecision::Deny,
                                    exit: -1,
                                    excerpt: "Command denied by Judge security policy".into(),
                                });
                                risks.push(format!(
                                    "Denied unsafe command execution: {}",
                                    argv.join(" ")
                                ));
                                messages.push(serde_json::json!({ "role": "user", "content": "Command denied by Judge security policy" }));
                            }
                            JudgeResult::Pause => {
                                final_status = TaskStatus::Blocked;
                                commands_executed.push(ReviewCommand {
                                    argv: argv.clone(),
                                    judge: JudgeDecision::Pause,
                                    exit: -1,
                                    excerpt: "Command execution paused pending approval".into(),
                                });
                                risks.push("Command requires human approval".into());
                                messages.push(serde_json::json!({ "role": "user", "content": "Command paused pending human approval" }));
                                should_stop = true;
                            }
                        }
                    }
                    _ => {
                        messages.push(serde_json::json!({ "role": "user", "content": format!("Unknown or unhandled tool: {}", tool_call.name) }));
                    }
                }

                if should_stop {
                    break;
                }
            }

            if should_stop {
                break;
            }
        }

        if run.step_count >= max_steps && final_status != TaskStatus::Blocked {
            risks.push("Step budget exhausted (max_steps)".into());
        }

        if cancel_token.is_cancelled() {
            return Err(anyhow!("Task execution cancelled by user"));
        }

        // Generate ReviewPacket from proposed/ tree vs live root
        let proposed_dir = store.get_task_proposed_dir(&project.root_path, &task.id);
        let mut rel_proposed_files = Vec::new();
        let _ = list_dir_relative(&proposed_dir, &proposed_dir, &mut rel_proposed_files);

        let mut diffs = Vec::new();
        for rel_path in rel_proposed_files {
            let proposed_file_path = proposed_dir.join(&rel_path);
            let proposed_content = fs::read_to_string(&proposed_file_path).unwrap_or_default();

            let original_content = if let Ok(live_path) =
                TaskRuntimeStore::validate_and_resolve_path(&project.root_path, &rel_path)
            {
                fs::read_to_string(live_path).unwrap_or_default()
            } else {
                "".to_string()
            };

            let unified_diff = format!(
                "--- a/{0}\\n+++ b/{0}\\n@@ -0,0 +1,3 @@\\n+{1}",
                rel_path,
                proposed_content.replace("\\n", "\\n+")
            );

            diffs.push(ReviewDiff {
                path: rel_path,
                unified_diff,
                original: original_content,
                proposed: proposed_content,
            });
        }

        let review = ReviewPacket {
            id: format!("rev_{}", Uuid::new_v4().simple()),
            task_id: task.id.clone(),
            run_id: run.id.clone(),
            summary: format!("Proposed changes for task: {}", task.goal),
            diffs,
            commands: commands_executed,
            checks,
            risks,
            created_at: Utc::now().to_rfc3339(),
        };

        Ok((review, final_status))
    }
}

#[cfg(test)]
pub struct FakeBackend {
    pub responses: Arc<Mutex<Vec<AgentResponse>>>,
}

#[cfg(test)]
#[async_trait::async_trait]
impl AgentBackend for FakeBackend {
    async fn chat(
        &self,
        _messages: &[serde_json::Value],
        _allowed_tools: &[String],
    ) -> Result<AgentResponse> {
        let mut guard = self.responses.lock().await;
        if guard.is_empty() {
            Ok(AgentResponse {
                content: Some("Finished".into()),
                tool_calls: vec![],
            })
        } else {
            Ok(guard.remove(0))
        }
    }
}

#[cfg(test)]
mod slice1_tests {
    use super::*;

    #[tokio::test]
    async fn test_fake_backend_write_file_and_acceptance() {
        let temp_dir = std::env::temp_dir().join(format!("ghostlink_slice1_{}", Uuid::new_v4()));
        let store = Arc::new(TaskRuntimeStore::new(&temp_dir).unwrap());

        let proj_dir =
            std::env::temp_dir().join(format!("ghostlink_slice1_proj_{}", Uuid::new_v4()));
        fs::create_dir_all(&proj_dir).unwrap();
        let root_str = proj_dir.to_string_lossy().to_string();

        let proj = store
            .create_project(
                "Slice1 Proj".into(),
                ProjectKind::Code,
                root_str.clone(),
                None,
                None,
            )
            .unwrap();
        let task = store
            .create_task(&proj.id, "Create output file".into(), None, None, None)
            .unwrap();

        let fake_backend = Arc::new(FakeBackend {
            responses: Arc::new(Mutex::new(vec![
                AgentResponse {
                    content: Some("Writing file".into()),
                    tool_calls: vec![ToolCall {
                        id: "tc_1".into(),
                        name: "write_file".into(),
                        args: serde_json::json!({
                            "path": "test_output.txt",
                            "content": "Hello Slice 1"
                        }),
                    }],
                },
                AgentResponse {
                    content: Some("All done".into()),
                    tool_calls: vec![],
                },
            ])),
        });

        let cancel_token = tokio_util::sync::CancellationToken::new();
        let mut run = AgentRun {
            id: format!("run_{}", Uuid::new_v4().simple()),
            task_id: task.id.clone(),
            role: "implementer".into(),
            model: "mock-model".into(),
            status: "running".into(),
            step_count: 0,
            token_count: 0,
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
            error: None,
        };

        let (review, final_status) = TaskRunner::run_loop(
            store.clone(),
            fake_backend,
            task.clone(),
            &mut run,
            None,
            cancel_token,
        )
        .await
        .unwrap();

        assert_eq!(final_status, TaskStatus::NeedsReview);
        assert_eq!(review.diffs.len(), 1);
        assert_eq!(review.diffs[0].path, "test_output.txt");

        let proposed_file = store
            .get_task_proposed_dir(&root_str, &task.id)
            .join("test_output.txt");
        let live_file = proj_dir.join("test_output.txt");

        assert!(proposed_file.exists());
        assert!(!live_file.exists());

        store.apply_proposed_changes(&root_str, &task.id).unwrap();
        assert!(live_file.exists());
        assert_eq!(fs::read_to_string(live_file).unwrap(), "Hello Slice 1");

        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::remove_dir_all(&proj_dir);
    }

    #[tokio::test]
    async fn test_judge_deny_tool_is_not_executed() {
        let temp_dir = std::env::temp_dir().join(format!("ghostlink_deny_{}", Uuid::new_v4()));
        let store = Arc::new(TaskRuntimeStore::new(&temp_dir).unwrap());

        let proj_dir =
            std::env::temp_dir().join(format!("ghostlink_deny_proj_{}", Uuid::new_v4()));
        fs::create_dir_all(&proj_dir).unwrap();

        let proj = store
            .create_project(
                "Deny Proj".into(),
                ProjectKind::Code,
                proj_dir.to_string_lossy().to_string(),
                None,
                None,
            )
            .unwrap();
        let task = store
            .create_task(&proj.id, "Try dangerous command".into(), None, None, None)
            .unwrap();

        let fake_backend = Arc::new(FakeBackend {
            responses: Arc::new(Mutex::new(vec![AgentResponse {
                content: Some("Exec rm".into()),
                tool_calls: vec![ToolCall {
                    id: "tc_deny".into(),
                    name: "run_command".into(),
                    args: serde_json::json!({
                        "argv": ["rm", "-rf", "/"]
                    }),
                }],
            }])),
        });

        let cancel_token = tokio_util::sync::CancellationToken::new();
        let mut run = AgentRun {
            id: format!("run_{}", Uuid::new_v4().simple()),
            task_id: task.id.clone(),
            role: "implementer".into(),
            model: "mock-model".into(),
            status: "running".into(),
            step_count: 0,
            token_count: 0,
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
            error: None,
        };

        let (review, _status) = TaskRunner::run_loop(
            store.clone(),
            fake_backend,
            task.clone(),
            &mut run,
            None,
            cancel_token,
        )
        .await
        .unwrap();

        assert_eq!(review.commands.len(), 1);
        assert_eq!(review.commands[0].judge, JudgeDecision::Deny);
        assert!(review
            .risks
            .iter()
            .any(|r| r.contains("Denied unsafe command")));

        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::remove_dir_all(&proj_dir);
    }

    #[tokio::test]
    async fn test_judge_pause_sets_blocked() {
        let temp_dir = std::env::temp_dir().join(format!("ghostlink_pause_{}", Uuid::new_v4()));
        let store = Arc::new(TaskRuntimeStore::new(&temp_dir).unwrap());

        let proj_dir =
            std::env::temp_dir().join(format!("ghostlink_pause_proj_{}", Uuid::new_v4()));
        fs::create_dir_all(&proj_dir).unwrap();

        let proj = store
            .create_project(
                "Pause Proj".into(),
                ProjectKind::Code,
                proj_dir.to_string_lossy().to_string(),
                None,
                None,
            )
            .unwrap();
        let task = store
            .create_task(&proj.id, "Try unknown command".into(), None, None, None)
            .unwrap();

        let fake_backend = Arc::new(FakeBackend {
            responses: Arc::new(Mutex::new(vec![AgentResponse {
                content: Some("Exec custom".into()),
                tool_calls: vec![ToolCall {
                    id: "tc_pause".into(),
                    name: "run_command".into(),
                    args: serde_json::json!({
                        "argv": ["custom_unknown_binary", "--arg"]
                    }),
                }],
            }])),
        });

        let cancel_token = tokio_util::sync::CancellationToken::new();
        let mut run = AgentRun {
            id: format!("run_{}", Uuid::new_v4().simple()),
            task_id: task.id.clone(),
            role: "implementer".into(),
            model: "mock-model".into(),
            status: "running".into(),
            step_count: 0,
            token_count: 0,
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
            error: None,
        };

        let (review, final_status) = TaskRunner::run_loop(
            store.clone(),
            fake_backend,
            task.clone(),
            &mut run,
            None,
            cancel_token,
        )
        .await
        .unwrap();

        assert_eq!(final_status, TaskStatus::Blocked);
        assert_eq!(review.commands.len(), 1);
        assert_eq!(review.commands[0].judge, JudgeDecision::Pause);

        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::remove_dir_all(&proj_dir);
    }

    #[tokio::test]
    async fn test_cancel_mid_loop_sets_cancelled() {
        let temp_dir = std::env::temp_dir().join(format!("ghostlink_cancel_{}", Uuid::new_v4()));
        let store = Arc::new(TaskRuntimeStore::new(&temp_dir).unwrap());

        let proj_dir =
            std::env::temp_dir().join(format!("ghostlink_cancel_proj_{}", Uuid::new_v4()));
        fs::create_dir_all(&proj_dir).unwrap();

        let proj = store
            .create_project(
                "Cancel Proj".into(),
                ProjectKind::Code,
                proj_dir.to_string_lossy().to_string(),
                None,
                None,
            )
            .unwrap();
        let task = store
            .create_task(&proj.id, "Cancel task".into(), None, None, None)
            .unwrap();

        let fake_backend = Arc::new(FakeBackend {
            responses: Arc::new(Mutex::new(vec![AgentResponse {
                content: Some("Step 1".into()),
                tool_calls: vec![],
            }])),
        });

        let cancel_token = tokio_util::sync::CancellationToken::new();
        cancel_token.cancel();

        let mut run = AgentRun {
            id: format!("run_{}", Uuid::new_v4().simple()),
            task_id: task.id.clone(),
            role: "implementer".into(),
            model: "mock-model".into(),
            status: "running".into(),
            step_count: 0,
            token_count: 0,
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
            error: None,
        };

        let res = TaskRunner::run_loop(
            store.clone(),
            fake_backend,
            task.clone(),
            &mut run,
            None,
            cancel_token,
        )
        .await;

        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("cancelled"));

        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::remove_dir_all(&proj_dir);
    }

    #[tokio::test]
    async fn test_budget_max_steps_stops_and_emits_review() {
        let temp_dir = std::env::temp_dir().join(format!("ghostlink_budget_{}", Uuid::new_v4()));
        let store = Arc::new(TaskRuntimeStore::new(&temp_dir).unwrap());

        let proj_dir =
            std::env::temp_dir().join(format!("ghostlink_budget_proj_{}", Uuid::new_v4()));
        fs::create_dir_all(&proj_dir).unwrap();

        let proj = store
            .create_project(
                "Budget Proj".into(),
                ProjectKind::Code,
                proj_dir.to_string_lossy().to_string(),
                None,
                None,
            )
            .unwrap();
        let budget = TaskBudget { max_steps: 1, ..Default::default() };

        let task = store
            .create_task(&proj.id, "Budget task".into(), None, Some(budget), None)
            .unwrap();

        let fake_backend = Arc::new(FakeBackend {
            responses: Arc::new(Mutex::new(vec![
                AgentResponse {
                    content: Some("Step 1".into()),
                    tool_calls: vec![ToolCall {
                        id: "tc_1".into(),
                        name: "read_file".into(),
                        args: serde_json::json!({ "path": "nonexistent.txt" }),
                    }],
                },
                AgentResponse {
                    content: Some("Step 2 - should not run".into()),
                    tool_calls: vec![],
                },
            ])),
        });

        let cancel_token = tokio_util::sync::CancellationToken::new();
        let mut run = AgentRun {
            id: format!("run_{}", Uuid::new_v4().simple()),
            task_id: task.id.clone(),
            role: "implementer".into(),
            model: "mock-model".into(),
            status: "running".into(),
            step_count: 0,
            token_count: 0,
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
            error: None,
        };

        let (review, _status) = TaskRunner::run_loop(
            store.clone(),
            fake_backend,
            task.clone(),
            &mut run,
            None,
            cancel_token,
        )
        .await
        .unwrap();

        assert_eq!(run.step_count, 1);
        assert!(review
            .risks
            .iter()
            .any(|r| r.contains("Step budget exhausted")));

        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::remove_dir_all(&proj_dir);
    }
}
"""

# Insert list_child_tasks into TaskRuntimeStore
pos_get_task = tr.find("pub fn get_task")
if pos_get_task != -1 and "list_child_tasks" not in tr:
    tr = tr[:pos_get_task] + list_child_tasks_code + "\n" + tr[pos_get_task:]

# Insert trait and runner code
pos_runner = tr.find("// Implementer Loop Runner")
if pos_runner == -1:
    pos_runner = tr.find("pub struct TaskRunner;")

test_pos = tr.find("#[cfg(test)]")
if pos_runner != -1 and test_pos != -1:
    tr = tr[:pos_runner] + trait_code + "\n" + runner_code + "\n" + tr[test_pos:]

with open("crates/ghost-link/src/task_runtime.rs", "w") as f:
    f.write(tr)

print("Updated task_runtime.rs")

# 2. Update task_api.rs
with open("crates/ghost-link/src/task_api.rs", "r") as f:
    ta = f.read()

task_api_fixed = """use anyhow::Result;
use crate::backend_plugin;
use crate::task_runtime::{AgentBackend, AgentResponse, ProjectKind, TaskBudget, TaskStatus, ToolCall};
use crate::BackendState;
use crate::InferenceEngine;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, Sse},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

pub struct RealAgentBackend {
    pub state: Arc<std::sync::Mutex<BackendState>>,
}

#[async_trait::async_trait]
impl AgentBackend for RealAgentBackend {
    async fn chat(
        &self,
        messages: &[serde_json::Value],
        _allowed_tools: &[String],
    ) -> Result<AgentResponse> {
        let (model, native_engine_client, ollama_client, vllm_client, settings, plugin_registry, inference_backend) = {
            let guard = self.state.lock().unwrap();
            (
                guard.current_model.clone(),
                guard.native_engine_client.clone(),
                guard.ollama_client.clone(),
                guard.vllm_client.clone(),
                guard.settings.clone(),
                guard.plugin_registry.clone(),
                guard.inference_backend,
            )
        };

        let mut prompt = String::new();
        for msg in messages {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
            if !prompt.is_empty() {
                prompt.push('\\n');
            }
            prompt.push_str(role);
            prompt.push_str(": ");
            prompt.push_str(content);
        }

        if let Some(plugin) = plugin_registry.get(&settings.inference_backend) {
            let res = plugin
                .generate(backend_plugin::PluginGenerationRequest {
                    model,
                    prompt,
                    temperature: 0.7,
                    top_p: 0.9,
                    top_k: 40,
                    penalty: 1.1,
                    max_tokens: 1024,
                })
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            return Ok(AgentResponse {
                content: Some(res.text),
                tool_calls: vec![],
            });
        }

        match inference_backend {
            InferenceEngine::Ollama => {
                let res_text = ollama_client
                    .generate(&model, &prompt, 0.7, 0.9, 40, 1.1, 1024)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(AgentResponse {
                    content: Some(res_text),
                    tool_calls: vec![],
                })
            }
            InferenceEngine::Vllm => {
                let res_text = vllm_client
                    .generate(&model, &prompt, 0.7, 0.9, 40, 1.1, 1024)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(AgentResponse {
                    content: Some(res_text),
                    tool_calls: vec![],
                })
            }
            InferenceEngine::Native => {
                let gen = native_engine_client
                    .generate(
                        &model,
                        &prompt,
                        1024,
                        0.7,
                        0.9,
                        40,
                        1.1,
                        &settings.native_engine,
                        &[],
                        None,
                        false,
                        None,
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;

                let mut tool_calls = Vec::new();
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&gen.text) {
                    if let Some(tc) = v.get("tool_call") {
                        if let (Some(name), Some(args)) = (tc.get("name").and_then(|n| n.as_str()), tc.get("args")) {
                            tool_calls.push(ToolCall {
                                id: format!("tc_{}", uuid::Uuid::new_v4().simple()),
                                name: name.to_string(),
                                args: args.clone(),
                            });
                        }
                    }
                }

                Ok(AgentResponse {
                    content: Some(gen.text),
                    tool_calls,
                })
            }
        }
    }
}
"""

pos_req = ta.find("#[derive(Deserialize)]\npub struct CreateProjectReq")
if pos_req == -1:
    pos_req = ta.find("pub struct CreateProjectReq")

rest_ta = ta[pos_req:]

# Add handle_get_task_children
children_handler = """
async fn handle_get_task_children(
    State(state): State<Arc<std::sync::Mutex<BackendState>>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let store = Arc::clone(&state.lock().unwrap().task_store);
    match store.list_child_tasks(&id) {
        Ok(children) => Ok(Json(serde_json::to_value(children).unwrap())),
        Err(e) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )),
    }
}
"""

pos_router = rest_ta.find("pub fn router()")
if pos_router != -1:
    rest_ta = rest_ta[:pos_router] + children_handler + "\n" + rest_ta[pos_router:]

old_spawn_call = """    match crate::task_runtime::TaskRunner::spawn_implementer(
        store,
        task,
        role,
        model,
        payload.brief,
        cancel_token,
    )"""

new_spawn_call = """    let backend = Arc::new(RealAgentBackend {
        state: Arc::clone(&state),
    });

    match crate::task_runtime::TaskRunner::spawn_implementer(
        store,
        backend,
        task,
        role,
        model,
        payload.brief,
        cancel_token,
    )"""

rest_ta = rest_ta.replace(old_spawn_call, new_spawn_call)

old_router_def = """.route("/api/tasks/:id", get(handle_get_task))"""
new_router_def = """.route("/api/tasks/:id", get(handle_get_task))\n        .route("/api/tasks/:id/children", get(handle_get_task_children))"""

rest_ta = rest_ta.replace(old_router_def, new_router_def)

with open("crates/ghost-link/src/task_api.rs", "w") as f:
    f.write(task_api_fixed + "\n\n" + rest_ta)

print("Updated task_api.rs")

# 3. Update ghostlink_gui_modern/src/api.ts
with open("ghostlink_gui_modern/src/api.ts", "r") as f:
    gui_api = f.read()

gui_api = gui_api.replace(
    "async createTask(projectId: string, data: { goal: string; acceptance_criteria?: string; budget?: Partial<TaskBudget>; model?: string }): Promise<Task> {",
    "async createTask(projectId: string, data: { goal: string; acceptance_criteria?: string; budget?: Partial<TaskBudget>; model?: string; parent_id?: string }): Promise<Task> {"
)

if "listTaskChildren" not in gui_api:
    get_children_method = """  async listTaskChildren(id: string): Promise<Task[]> {
    const res = await this.http.get(`/api/tasks/${id}/children`);
    return res.data;
  }
"""
    pos_gt = gui_api.find("async getTask(")
    if pos_gt != -1:
        gui_api = gui_api[:pos_gt] + get_children_method + "\n  " + gui_api[pos_gt:]

with open("ghostlink_gui_modern/src/api.ts", "w") as f:
    f.write(gui_api)

print("Updated ghostlink_gui_modern/src/api.ts")

# 4. Update ProjectsTab.tsx
with open("ghostlink_gui_modern/src/components/ProjectsTab.tsx", "r") as f:
    pt = f.read()

pt = pt.replace(
    "activeTask?.id === t.id ? 'bg-indigo-600/20 text-indigo-300 border border-indigo-500/30' : 'text-slate-300 hover:bg-slate-800'",
    "${t.parent_id ? 'ml-3 border-l-2 border-indigo-500/50 pl-2.5' : ''} ${activeTask?.id === t.id ? 'bg-indigo-600/20 text-indigo-300 border border-indigo-500/30' : 'text-slate-300 hover:bg-slate-800'}"
)

with open("ghostlink_gui_modern/src/components/ProjectsTab.tsx", "w") as f:
    f.write(pt)

print("Updated ProjectsTab.tsx")

# 5. Update docs/TASK_AGENTS.md & CHANGELOG.md
with open("docs/TASK_AGENTS.md", "r") as f:
    docs = f.read()

docs_updated = docs.replace(
    "- **AgentRun**: `id`, `task_id`, `role` (`implementer`), `model`, `status`, `step_count`, `token_count`, `started_at`, `finished_at?`, `error?`",
    "- **AgentRun**: `id`, `task_id`, `role` (`implementer` | `planner`), `model`, `status`, `step_count`, `token_count`, `started_at`, `finished_at?`, `error?`"
).replace(
    "GET    /api/tasks/:id                           # Get task details",
    "GET    /api/tasks/:id                           # Get task details\nGET    /api/tasks/:id/children                  # Get child tasks for parent task"
).replace(
    "### Implemented in v2.3 Phase A Server:",
    "### Implemented in v2.4 Task Agents & Fan-out:"
).replace(
    "### Out of Scope / Planned for v2.4:\n- Child task fan-out trees with parent accept blocking and cascading cancellations.",
    "### Implemented v2.4 Capabilities:\n- **Bounded Agent Tool Loop**: Real bounded tool-calling loop using in-process `AgentBackend` and OpenAI-compatible inference with deterministic `Judge` policy evaluation, staged file mutations in `.ghostlink/tasks/<id>/proposed/`, real shell tool execution, and budget controls (`max_steps`, `max_tokens`, `max_minutes`).\n- **v2.4 Child Fan-Out Trees**: Hierarchical child task creation (`parent_id`), budget inheritance, parent accept blocking (`check_parent_accept_allowed`), child task listing (`/api/tasks/:id/children`), and `planner` vs `implementer` role enforcement."
)

with open("docs/TASK_AGENTS.md", "w") as f:
    f.write(docs_updated)

with open("CHANGELOG.md", "r") as f:
    changelog = f.read()

if "Real Task Agent Tool Loop & v2.4 Child Fan-Out" not in changelog:
    entry = """### Added
- **Real Task Agent Tool Loop & v2.4 Child Fan-Out**: Replaced stub canned implementer loop with bounded tool loop using in-process `AgentBackend` trait, `Judge` policy enforcement, proposed file staging in `.ghostlink/tasks/<id>/proposed/`, real command execution, child task fan-out APIs (`/api/tasks/:id/children`), budget inheritance, and UI child task representation. (`crates/ghost-link/src/task_runtime.rs`, `crates/ghost-link/src/task_api.rs`, `ghostlink_gui_modern/src/components/TaskView.tsx`, `ghostlink_gui_modern/src/components/ProjectsTab.tsx`)

"""
    pos = changelog.find("## [Unreleased]")
    if pos != -1:
        pos_added = changelog.find("\n", pos)
        changelog = changelog[:pos_added+1] + "\n" + entry + changelog[pos_added+1:]
        with open("CHANGELOG.md", "w") as f:
            f.write(changelog)

print("Applied all changes")
