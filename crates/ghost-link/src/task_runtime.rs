use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    Code,
    Work,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub kind: ProjectKind,
    pub root_path: String,
    pub allowed_tools: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    pub created_at: String,
}

impl Project {
    pub fn effective_allowed_tools(&self) -> Vec<String> {
        if self.allowed_tools.is_empty() && self.kind == ProjectKind::Code {
            vec![
                "write_file".to_string(),
                "read_file".to_string(),
                "run_command".to_string(),
            ]
        } else {
            self.allowed_tools.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskBudget {
    pub max_steps: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    pub max_minutes: u32,
}

impl Default for TaskBudget {
    fn default() -> Self {
        Self {
            max_steps: 30,
            max_tokens: Some(100_000),
            max_minutes: 15,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    NeedsReview,
    Accepted,
    Rejected,
    Blocked,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub goal: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<String>,
    pub status: TaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub budget: TaskBudget,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRun {
    pub id: String,
    pub task_id: String,
    pub role: String,
    pub model: String,
    pub status: String,
    pub step_count: u32,
    pub token_count: u32,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewDiff {
    pub path: String,
    pub unified_diff: String,
    pub original: String,
    pub proposed: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JudgeDecision {
    Allow,
    Deny,
    Pause,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCommand {
    pub argv: Vec<String>,
    pub judge: JudgeDecision,
    pub exit: i32,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPacket {
    pub id: String,
    pub task_id: String,
    pub run_id: String,
    pub summary: String,
    pub diffs: Vec<ReviewDiff>,
    pub commands: Vec<ReviewCommand>,
    pub checks: Vec<String>,
    pub risks: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEvent {
    pub task_id: String,
    pub ts: u64,
    pub kind: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeResult {
    Allow,
    Deny,
    Pause,
}

pub struct Judge;

impl Judge {
    pub fn evaluate(argv: &[String]) -> JudgeResult {
        if argv.is_empty() {
            return JudgeResult::Deny;
        }

        let cmd_str = argv.join(" ");

        // Check explicit DENY rules first
        if cmd_str.contains("rm -rf")
            || cmd_str.contains("mkfs")
            || cmd_str.contains("dd ")
            || cmd_str.contains("format ")
            || cmd_str.contains("git reset --hard")
            || cmd_str.contains("git push --force")
            || cmd_str.contains("git push -f")
            || cmd_str.contains(".git/objects")
            || cmd_str.contains(".env")
            || cmd_str.contains("id_rsa")
        {
            return JudgeResult::Deny;
        }

        let first = argv[0].as_str();
        if first == "curl"
            || first == "wget"
            || first == "scp"
            || first == "ssh"
            || first == "sudo"
            || first == "su"
            || first == "docker"
            || first == "kubectl"
        {
            return JudgeResult::Deny;
        }

        // ALLOW table check
        if Self::is_allowed(argv) {
            return JudgeResult::Allow;
        }

        // Default to PAUSE for unrecognized commands
        JudgeResult::Pause
    }

    fn is_allowed(argv: &[String]) -> bool {
        if argv.is_empty() {
            return false;
        }

        let prog = argv[0].as_str();
        match prog {
            "ls" | "rg" | "grep" => true,
            "git" => {
                if argv.len() >= 2 {
                    matches!(argv[1].as_str(), "status" | "diff" | "log")
                } else {
                    false
                }
            }
            "cargo" => {
                if argv.len() >= 2 {
                    matches!(argv[1].as_str(), "test" | "check" | "clippy")
                } else {
                    false
                }
            }
            "npx" => {
                argv.len() >= 3
                    && ((argv[1] == "vitest" && argv[2] == "run")
                        || (argv[1] == "tsc" && argv[2] == "--noEmit"))
            }
            "python" | "python3" => argv.len() >= 3 && argv[1] == "-m" && argv[2] == "pytest",
            _ => false,
        }
    }
}

#[derive(Debug)]
pub struct TaskRuntimeStore {
    data_dir: PathBuf,
    event_bus: broadcast::Sender<TaskEvent>,
    active_runs: Mutex<HashMap<String, tokio_util::sync::CancellationToken>>,
}

impl TaskRuntimeStore {
    pub fn new<P: AsRef<Path>>(data_dir: P) -> Result<Self> {
        let path = data_dir.as_ref().to_path_buf();
        fs::create_dir_all(&path)?;
        fs::create_dir_all(path.join("projects"))?;
        fs::create_dir_all(path.join("tasks"))?;
        fs::create_dir_all(path.join("runs"))?;
        fs::create_dir_all(path.join("reviews"))?;

        fs::create_dir_all(path.join("events"))?;
        let (event_bus, _) = broadcast::channel(64);

        Ok(Self {
            data_dir: path,
            event_bus,
            active_runs: Mutex::new(HashMap::new()),
        })
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<TaskEvent> {
        self.event_bus.subscribe()
    }

    pub fn emit_event(&self, event: TaskEvent) {
        let events_dir = self.data_dir.join("events");
        let file_path = events_dir.join(format!("{}.jsonl", event.task_id));
        if let Ok(json) = serde_json::to_string(&event) {
            use std::io::Write;
            if let Ok(mut file) = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file_path)
            {
                let _ = writeln!(file, "{}", json);
            }
        }
        let _ = self.event_bus.send(event);
    }

    pub fn get_task_events(&self, task_id: &str) -> Vec<TaskEvent> {
        let file_path = self
            .data_dir
            .join("events")
            .join(format!("{}.jsonl", task_id));
        let mut events = Vec::new();
        if file_path.exists() {
            if let Ok(content) = fs::read_to_string(file_path) {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        if let Ok(ev) = serde_json::from_str::<TaskEvent>(trimmed) {
                            events.push(ev);
                        }
                    }
                }
            }
        }
        events
    }

    pub fn register_cancel_token(
        &self,
        task_id: String,
        token: tokio_util::sync::CancellationToken,
    ) {
        if let Ok(mut guard) = self.active_runs.try_lock() {
            guard.insert(task_id, token);
        }
    }

    pub fn cancel_task_run(&self, task_id: &str) -> bool {
        let mut cancelled = false;
        if let Ok(mut guard) = self.active_runs.try_lock() {
            if let Some(token) = guard.remove(task_id) {
                token.cancel();
                cancelled = true;
            }
        }

        // Cascade cancel to children if task_id has children
        if let Ok(task) = self.get_task(task_id) {
            if let Ok(tasks) = self.list_project_tasks(&task.project_id) {
                for child in tasks
                    .iter()
                    .filter(|t| t.parent_id.as_deref() == Some(task_id))
                {
                    let _ = self.cancel_task_run(&child.id);
                    let _ = self.update_task_status(&child.id, TaskStatus::Cancelled);
                }
            }
        }

        cancelled
    }

    fn atomic_write_json<T: Serialize>(path: &Path, val: &T) -> Result<()> {
        let temp_path = path.with_extension(format!("tmp.{}", Uuid::new_v4()));
        let json = serde_json::to_string_pretty(val)?;
        fs::write(&temp_path, json)?;
        fs::rename(&temp_path, path)?;
        Ok(())
    }

    // Projects
    pub fn create_project(
        &self,
        name: String,
        kind: ProjectKind,
        root_path: String,
        allowed_tools: Option<Vec<String>>,
        default_model: Option<String>,
    ) -> Result<Project> {
        let canonical_root = fs::canonicalize(&root_path).with_context(|| {
            format!(
                "Project root_path '{}' does not exist or is invalid",
                root_path
            )
        })?;
        if !canonical_root.is_dir() {
            return Err(anyhow!(
                "Project root_path '{}' is not a directory",
                root_path
            ));
        }

        let proj = Project {
            id: format!("proj_{}", Uuid::new_v4().simple()),
            name,
            kind,
            root_path: canonical_root.to_string_lossy().to_string(),
            allowed_tools: allowed_tools.unwrap_or_else(|| {
                vec![
                    "read_file".into(),
                    "write_file".into(),
                    "run_command".into(),
                ]
            }),
            default_model,
            created_at: Utc::now().to_rfc3339(),
        };

        let file_path = self
            .data_dir
            .join("projects")
            .join(format!("{}.json", proj.id));
        Self::atomic_write_json(&file_path, &proj)?;
        Ok(proj)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let dir = self.data_dir.join("projects");
        let mut projects = Vec::new();
        if dir.is_dir() {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    let content = fs::read_to_string(&path)?;
                    if let Ok(proj) = serde_json::from_str::<Project>(&content) {
                        projects.push(proj);
                    }
                }
            }
        }
        projects.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(projects)
    }

    pub fn get_project(&self, id: &str) -> Result<Project> {
        let file_path = self.data_dir.join("projects").join(format!("{}.json", id));
        if !file_path.exists() {
            return Err(anyhow!("Project '{}' not found", id));
        }
        let content = fs::read_to_string(file_path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn update_project(
        &self,
        id: &str,
        name: Option<String>,
        default_model: Option<String>,
        allowed_tools: Option<Vec<String>>,
    ) -> Result<Project> {
        let mut proj = self.get_project(id)?;
        if let Some(n) = name {
            proj.name = n;
        }
        if let Some(m) = default_model {
            proj.default_model = Some(m);
        }
        if let Some(t) = allowed_tools {
            proj.allowed_tools = t;
        }
        let file_path = self.data_dir.join("projects").join(format!("{}.json", id));
        Self::atomic_write_json(&file_path, &proj)?;
        Ok(proj)
    }

    // Tasks
    pub fn create_task(
        &self,
        project_id: &str,
        goal: String,
        acceptance_criteria: Option<String>,
        budget: Option<TaskBudget>,
        parent_id: Option<String>,
    ) -> Result<Task> {
        let _proj = self.get_project(project_id)?;

        // Phase B Fan-Out caps: max_depth = 1, max_children = 4
        if let Some(ref p_id) = parent_id {
            let parent_task = self.get_task(p_id)?;
            if parent_task.parent_id.is_some() {
                return Err(anyhow!("Child task depth limit exceeded (max_depth=1)"));
            }

            let existing_tasks = self.list_project_tasks(project_id)?;
            let child_count = existing_tasks
                .iter()
                .filter(|t| t.parent_id.as_deref() == Some(p_id.as_str()))
                .count();
            if child_count >= 4 {
                return Err(anyhow!(
                    "Child task count limit exceeded for parent (max_children=4)"
                ));
            }
        }

        let now = Utc::now().to_rfc3339();
        let task = Task {
            id: format!("task_{}", Uuid::new_v4().simple()),
            project_id: project_id.to_string(),
            goal,
            acceptance_criteria,
            status: TaskStatus::Queued,
            parent_id,
            budget: budget.unwrap_or_default(),
            created_at: now.clone(),
            updated_at: now,
        };

        let file_path = self
            .data_dir
            .join("tasks")
            .join(format!("{}.json", task.id));
        Self::atomic_write_json(&file_path, &task)?;
        Ok(task)
    }

    pub fn list_project_tasks(&self, project_id: &str) -> Result<Vec<Task>> {
        let dir = self.data_dir.join("tasks");
        let mut tasks = Vec::new();
        if dir.is_dir() {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    let content = fs::read_to_string(&path)?;
                    if let Ok(task) = serde_json::from_str::<Task>(&content) {
                        if task.project_id == project_id {
                            tasks.push(task);
                        }
                    }
                }
            }
        }
        tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(tasks)
    }

    pub fn list_child_tasks(&self, parent_id: &str) -> Result<Vec<Task>> {
        let parent = self.get_task(parent_id)?;
        let all_tasks = self.list_project_tasks(&parent.project_id)?;
        Ok(all_tasks
            .into_iter()
            .filter(|t| t.parent_id.as_deref() == Some(parent_id))
            .collect())
    }

    pub fn get_task(&self, id: &str) -> Result<Task> {
        let file_path = self.data_dir.join("tasks").join(format!("{}.json", id));
        if !file_path.exists() {
            return Err(anyhow!("Task '{}' not found", id));
        }
        let content = fs::read_to_string(file_path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn update_task_status(&self, id: &str, status: TaskStatus) -> Result<Task> {
        let mut task = self.get_task(id)?;
        task.status = status;
        task.updated_at = Utc::now().to_rfc3339();
        let file_path = self.data_dir.join("tasks").join(format!("{}.json", id));
        Self::atomic_write_json(&file_path, &task)?;
        Ok(task)
    }

    pub fn requeue_task(&self, id: &str) -> Result<Task> {
        let mut task = self.get_task(id)?;
        task.status = TaskStatus::Queued;
        task.updated_at = Utc::now().to_rfc3339();
        let file_path = self.data_dir.join("tasks").join(format!("{}.json", id));
        Self::atomic_write_json(&file_path, &task)?;
        Ok(task)
    }

    // Runs
    pub fn save_run(&self, run: &AgentRun) -> Result<()> {
        let file_path = self.data_dir.join("runs").join(format!("{}.json", run.id));
        Self::atomic_write_json(&file_path, run)
    }

    #[allow(dead_code)]
    pub fn get_run(&self, id: &str) -> Result<AgentRun> {
        let file_path = self.data_dir.join("runs").join(format!("{}.json", id));
        if !file_path.exists() {
            return Err(anyhow!("Run '{}' not found", id));
        }
        let content = fs::read_to_string(file_path)?;
        Ok(serde_json::from_str(&content)?)
    }

    // Reviews
    pub fn save_review(&self, review: &ReviewPacket) -> Result<()> {
        let file_path = self
            .data_dir
            .join("reviews")
            .join(format!("{}.json", review.id));
        Self::atomic_write_json(&file_path, review)?;
        // Also save by task_id for quick lookup
        let task_review_path = self
            .data_dir
            .join("reviews")
            .join(format!("task_{}.json", review.task_id));
        Self::atomic_write_json(&task_review_path, review)
    }

    pub fn get_task_review(&self, task_id: &str) -> Result<ReviewPacket> {
        let task_review_path = self
            .data_dir
            .join("reviews")
            .join(format!("task_{}.json", task_id));
        if !task_review_path.exists() {
            return Err(anyhow!("No review packet found for task '{}'", task_id));
        }
        let content = fs::read_to_string(task_review_path)?;
        Ok(serde_json::from_str(&content)?)
    }

    pub fn get_review(&self, id: &str) -> Result<ReviewPacket> {
        let file_path = self.data_dir.join("reviews").join(format!("{}.json", id));
        if !file_path.exists() {
            // Check if id is actually a task_id
            return self.get_task_review(id);
        }
        let content = fs::read_to_string(file_path)?;
        Ok(serde_json::from_str(&content)?)
    }

    // Path Isolation & Staging Helpers
    pub fn validate_and_resolve_path(root_path: &str, relative_path: &str) -> Result<PathBuf> {
        let root = PathBuf::from(root_path);
        let target = root.join(relative_path);

        // Normalize path without requiring target file to exist yet
        let mut normalized = PathBuf::new();
        for component in target.components() {
            match component {
                std::path::Component::ParentDir => {
                    if !normalized.pop() {
                        return Err(anyhow!("Path traversal detected in '{}'", relative_path));
                    }
                }
                std::path::Component::CurDir => {}
                c => normalized.push(c),
            }
        }

        let canonical_root =
            fs::canonicalize(&root).with_context(|| "Failed to canonicalize root_path")?;

        // Ensure normalized target stays under root
        if !normalized.starts_with(&canonical_root) && !normalized.starts_with(&root) {
            return Err(anyhow!("Path '{}' escapes project root", relative_path));
        }

        Ok(normalized)
    }

    pub fn get_task_proposed_dir(&self, project_root: &str, task_id: &str) -> PathBuf {
        PathBuf::from(project_root)
            .join(".ghostlink")
            .join("tasks")
            .join(task_id)
            .join("proposed")
    }

    pub fn write_proposed_file(
        &self,
        project_root: &str,
        task_id: &str,
        relative_path: &str,
        content: &str,
    ) -> Result<PathBuf> {
        // Prevent path traversal
        let target_live_path = Self::validate_and_resolve_path(project_root, relative_path)?;
        let root = PathBuf::from(project_root);
        let rel = target_live_path
            .strip_prefix(&root)
            .map_err(|_| anyhow!("Target path outside root"))?;

        let proposed_dir = self.get_task_proposed_dir(project_root, task_id);
        let proposed_file_path = proposed_dir.join(rel);

        if let Some(parent) = proposed_file_path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(&proposed_file_path, content)?;
        Ok(proposed_file_path)
    }

    pub fn check_parent_accept_allowed(
        &self,
        project_id: &str,
        parent_task_id: &str,
    ) -> Result<()> {
        let tasks = self.list_project_tasks(project_id)?;
        let unaccepted_children: Vec<_> = tasks
            .into_iter()
            .filter(|t| t.parent_id.as_deref() == Some(parent_task_id))
            .filter(|t| !matches!(t.status, TaskStatus::Accepted | TaskStatus::Cancelled))
            .map(|t| t.id)
            .collect();

        if !unaccepted_children.is_empty() {
            return Err(anyhow!("Parent task accept is blocked until all child tasks are accepted or cancelled (open children: {})", unaccepted_children.join(", ")));
        }
        Ok(())
    }

    pub fn apply_proposed_changes(&self, project_root: &str, task_id: &str) -> Result<Vec<String>> {
        let proposed_dir = self.get_task_proposed_dir(project_root, task_id);
        let mut applied_files = Vec::new();

        if !proposed_dir.exists() {
            return Ok(applied_files);
        }

        let root = PathBuf::from(project_root);

        fn visit_dirs(
            dir: &Path,
            proposed_root: &Path,
            live_root: &Path,
            applied: &mut Vec<String>,
        ) -> Result<()> {
            if dir.is_dir() {
                for entry in fs::read_dir(dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_dir() {
                        visit_dirs(&path, proposed_root, live_root, applied)?;
                    } else {
                        let rel = path.strip_prefix(proposed_root)?;
                        let live_target = live_root.join(rel);
                        if let Some(parent) = live_target.parent() {
                            fs::create_dir_all(parent)?;
                        }
                        fs::copy(&path, &live_target)?;
                        applied.push(rel.to_string_lossy().to_string());
                    }
                }
            }
            Ok(())
        }

        visit_dirs(&proposed_dir, &proposed_dir, &root, &mut applied_files)?;

        // Clean up proposed directory after successful apply
        let _ = fs::remove_dir_all(&proposed_dir);

        Ok(applied_files)
    }

    pub fn discard_proposed_changes(&self, project_root: &str, task_id: &str) -> Result<()> {
        let proposed_dir = self.get_task_proposed_dir(project_root, task_id);
        if proposed_dir.exists() {
            fs::remove_dir_all(&proposed_dir)?;
        }
        Ok(())
    }
}

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
fn list_dir_relative(
    base_dir: &Path,
    current_dir: &Path,
    rel_paths: &mut Vec<String>,
) -> Result<()> {
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

// Implementer Loop Runner
pub struct TaskRunner;

impl TaskRunner {
    #[allow(dead_code)]
    pub async fn spawn_implementer_joined(
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

        let mut run_clone = run.clone();
        let res = Self::run_loop(
            store.clone(),
            backend,
            task.clone(),
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
                let _ = store.save_run(&run_clone);
                let _ = store.save_review(&review);
                let _ = store.update_task_status(&task.id, final_status);

                store.emit_event(TaskEvent {
                    task_id: task.id.clone(),
                    ts: Utc::now().timestamp_millis() as u64,
                    kind: "review_ready".into(),
                    payload: serde_json::to_value(&review).unwrap_or_default(),
                });
                Ok(run_clone)
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
                let _ = store.save_run(&run_clone);

                let status = if is_cancelled {
                    TaskStatus::Cancelled
                } else {
                    TaskStatus::Blocked
                };
                let _ = store.update_task_status(&task.id, status);

                store.emit_event(TaskEvent {
                    task_id: task.id.clone(),
                    ts: Utc::now().timestamp_millis() as u64,
                    kind: if is_cancelled {
                        "cancelled".into()
                    } else {
                        "error".into()
                    },
                    payload: serde_json::json!({ "error": err_msg }),
                });

                Err(anyhow!(err_msg))
            }
        }
    }
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

    #[allow(clippy::type_complexity)]
    fn run_loop<'a>(
        store: Arc<TaskRuntimeStore>,
        backend: Arc<dyn AgentBackend>,
        task: Task,
        run: &'a mut AgentRun,
        brief: Option<String>,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(ReviewPacket, TaskStatus)>> + Send + 'a>,
    > {
        Box::pin(async move {
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

            let mut user_prompt = format!("Task Goal: {}\n", task.goal);
            if let Some(b) = &brief {
                user_prompt.push_str(&format!("Execution Brief: {}\n", b));
            }
            if let Some(ac) = &task.acceptance_criteria {
                user_prompt.push_str(&format!("Acceptance Criteria: {}\n", ac));
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
                let agent_resp = match backend
                    .chat(&messages, &project.effective_allowed_tools())
                    .await
                {
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
                    messages
                        .push(serde_json::json!({ "role": "assistant", "content": content_str }));
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
                                risks.push(
                                    "Implementer agent tried to spawn subagent (denied)".into(),
                                );
                                messages.push(serde_json::json!({ "role": "user", "content": "Error: implementer agents cannot spawn subagents" }));
                                continue;
                            }

                            let child_goal = tool_call
                                .args
                                .get("goal")
                                .and_then(|g| g.as_str())
                                .unwrap_or("");
                            let child_criteria = tool_call
                                .args
                                .get("acceptance_criteria")
                                .and_then(|c| c.as_str());

                            if child_goal.is_empty() {
                                messages.push(serde_json::json!({ "role": "user", "content": "Error: goal cannot be empty for child task" }));
                                continue;
                            }

                            let elapsed_mins = (start_time.elapsed().as_secs() / 60) as u32;
                            let rem_mins =
                                task.budget.max_minutes.saturating_sub(elapsed_mins).max(1);
                            let rem_steps =
                                task.budget.max_steps.saturating_sub(run.step_count).max(1);
                            let rem_tokens = task
                                .budget
                                .max_tokens
                                .map(|mt| mt.saturating_sub(run.token_count));

                            let child_budget = TaskBudget {
                                max_steps: rem_steps,
                                max_tokens: rem_tokens,
                                max_minutes: rem_mins,
                            };

                            match store.create_task(
                                &project.id,
                                child_goal.to_string(),
                                child_criteria.map(|s| s.to_string()),
                                Some(child_budget),
                                Some(task.id.clone()),
                            ) {
                                Ok(child) => {
                                    store.emit_event(TaskEvent {
                                    task_id: task.id.clone(),
                                    ts: Utc::now().timestamp_millis() as u64,
                                    kind: "child_created".into(),
                                    payload: serde_json::json!({ "child_id": child.id, "goal": child.goal }),
                                });

                                    // Auto-spawn child task as implementer run with inherited cancel token
                                    let child_store = Arc::clone(&store);
                                    let child_backend = Arc::clone(&backend);
                                    let child_task = child.clone();
                                    let child_model = run.model.clone();
                                    let child_token = cancel_token.child_token();

                                    tokio::spawn(Box::pin(async move {
                                        let _ = Self::spawn_implementer(
                                            child_store,
                                            child_backend,
                                            child_task,
                                            "implementer".into(),
                                            child_model,
                                            None,
                                            child_token,
                                        )
                                        .await;
                                    }));

                                    messages.push(serde_json::json!({ "role": "user", "content": format!("Successfully created child task #{} ({})", child.id, child.goal) }));
                                }
                                Err(e) => {
                                    risks.push(format!("Failed to create child task: {}", e));
                                    messages.push(serde_json::json!({ "role": "user", "content": format!("Failed to create child task: {}", e) }));
                                }
                            }
                        }
                        "write_file" => {
                            let rel_path = tool_call
                                .args
                                .get("path")
                                .and_then(|p| p.as_str())
                                .unwrap_or("");
                            let file_content = tool_call
                                .args
                                .get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or("");

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

                            match store.write_proposed_file(
                                &project.root_path,
                                &task.id,
                                rel_path,
                                file_content,
                            ) {
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
                                    risks.push(format!(
                                        "Failed writing proposed file {}: {}",
                                        rel_path, e
                                    ));
                                    messages.push(serde_json::json!({ "role": "user", "content": format!("Error writing proposed file {}: {}", rel_path, e) }));
                                }
                            }
                        }
                        "read_file" => {
                            let rel_path = tool_call
                                .args
                                .get("path")
                                .and_then(|p| p.as_str())
                                .unwrap_or("");
                            let proposed_file = store
                                .get_task_proposed_dir(&project.root_path, &task.id)
                                .join(rel_path);
                            let file_text = if proposed_file.exists() {
                                fs::read_to_string(&proposed_file).unwrap_or_default()
                            } else if let Ok(live_p) = TaskRuntimeStore::validate_and_resolve_path(
                                &project.root_path,
                                rel_path,
                            ) {
                                fs::read_to_string(&live_p).unwrap_or_default()
                            } else {
                                "File not found".to_string()
                            };
                            messages.push(serde_json::json!({ "role": "user", "content": format!("File content of {}:\n{}", rel_path, file_text) }));
                        }
                        "run_command" | "exec" | "execute" | "shell" => {
                            let argv: Vec<String> = if let Some(arr) =
                                tool_call.args.get("argv").and_then(|a| a.as_array())
                            {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                    .collect()
                            } else if let Some(cmd_str) =
                                tool_call.args.get("command").and_then(|c| c.as_str())
                            {
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
                                    if !project.allowed_tools.contains(&"run_command".to_string())
                                        && !project.allowed_tools.contains(&"execute".to_string())
                                    {
                                        risks.push(
                                            "Command execution tool not allowed by project policy"
                                                .into(),
                                        );
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
                                            let excerpt =
                                                format!("STDOUT:\n{}\nSTDERR:\n{}", stdout, stderr);
                                            let trimmed_excerpt = if excerpt.len() > 1000 {
                                                format!("{}...\n(truncated)", &excerpt[..1000])
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
                                                risks.push(format!(
                                                    "Command failed (exit {}): {}",
                                                    exit_code,
                                                    argv.join(" ")
                                                ));
                                            }

                                            messages.push(serde_json::json!({
                                            "role": "user",
                                            "content": format!("Command '{}' exited with {}:\n{}", argv.join(" "), exit_code, trimmed_excerpt)
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
                    "--- a/{0}\n+++ b/{0}\n@@ -0,0 +1,3 @@\n+{1}",
                    rel_path,
                    proposed_content.replace("\n", "\n+")
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
        })
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

        let proj_dir = std::env::temp_dir().join(format!("ghostlink_deny_proj_{}", Uuid::new_v4()));
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
        let budget = TaskBudget {
            max_steps: 1,
            ..Default::default()
        };

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_roundtrip_and_atomic_write() {
        let temp_dir =
            std::env::temp_dir().join(format!("ghostlink_test_{}", uuid::Uuid::new_v4()));
        let store = TaskRuntimeStore::new(&temp_dir).unwrap();

        let proj_dir =
            std::env::temp_dir().join(format!("ghostlink_proj_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&proj_dir).unwrap();

        let proj = store
            .create_project(
                "Test Proj".into(),
                ProjectKind::Code,
                proj_dir.to_string_lossy().to_string(),
                None,
                None,
            )
            .unwrap();

        assert_eq!(proj.name, "Test Proj");
        let fetched_proj = store.get_project(&proj.id).unwrap();
        assert_eq!(fetched_proj.id, proj.id);

        let task = store
            .create_task(&proj.id, "Build feature X".into(), None, None, None)
            .unwrap();
        assert_eq!(task.goal, "Build feature X");
        assert_eq!(task.status, TaskStatus::Queued);

        let fetched_task = store.get_task(&task.id).unwrap();
        assert_eq!(fetched_task.id, task.id);

        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = std::fs::remove_dir_all(&proj_dir);
    }

    #[test]
    fn test_path_traversal_rejection() {
        let root_dir =
            std::env::temp_dir().join(format!("ghostlink_root_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root_dir).unwrap();
        let root = root_dir.to_string_lossy().to_string();

        let valid = TaskRuntimeStore::validate_and_resolve_path(&root, "src/main.rs");
        assert!(valid.is_ok());

        let invalid = TaskRuntimeStore::validate_and_resolve_path(&root, "../../../etc/passwd");
        assert!(invalid.is_err());

        let _ = std::fs::remove_dir_all(&root_dir);
    }

    #[test]
    fn test_judge_policy() {
        assert_eq!(
            Judge::evaluate(&["ls".into(), "-la".into()]),
            JudgeResult::Allow
        );
        assert_eq!(
            Judge::evaluate(&["cargo".into(), "test".into()]),
            JudgeResult::Allow
        );
        assert_eq!(
            Judge::evaluate(&["git".into(), "status".into()]),
            JudgeResult::Allow
        );

        assert_eq!(
            Judge::evaluate(&["rm".into(), "-rf".into(), "/".into()]),
            JudgeResult::Deny
        );
        assert_eq!(
            Judge::evaluate(&["curl".into(), "http://example.com".into()]),
            JudgeResult::Deny
        );
        assert_eq!(
            Judge::evaluate(&["sudo".into(), "reboot".into()]),
            JudgeResult::Deny
        );

        assert_eq!(
            Judge::evaluate(&["custom_tool".into(), "arg".into()]),
            JudgeResult::Pause
        );
    }

    #[test]
    fn test_accept_reject_and_request_changes() {
        let temp_dir =
            std::env::temp_dir().join(format!("ghostlink_test_{}", uuid::Uuid::new_v4()));
        let store = TaskRuntimeStore::new(&temp_dir).unwrap();

        let proj_dir =
            std::env::temp_dir().join(format!("ghostlink_proj_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&proj_dir).unwrap();
        let root_str = proj_dir.to_string_lossy().to_string();

        let proj = store
            .create_project(
                "Sample".into(),
                ProjectKind::Code,
                root_str.clone(),
                None,
                None,
            )
            .unwrap();
        let task = store
            .create_task(&proj.id, "Add file".into(), None, None, None)
            .unwrap();

        // Write proposed file
        store
            .write_proposed_file(&root_str, &task.id, "hello.txt", "Hello World")
            .unwrap();

        let live_file = proj_dir.join("hello.txt");
        assert!(!live_file.exists());

        // Reject test
        store.discard_proposed_changes(&root_str, &task.id).unwrap();
        let proposed_dir = store.get_task_proposed_dir(&root_str, &task.id);
        assert!(!proposed_dir.exists());

        // Re-write proposed file & Accept test
        store
            .write_proposed_file(&root_str, &task.id, "hello.txt", "Hello World")
            .unwrap();
        let applied = store.apply_proposed_changes(&root_str, &task.id).unwrap();
        assert_eq!(applied, vec!["hello.txt"]);
        assert!(live_file.exists());
        assert_eq!(std::fs::read_to_string(&live_file).unwrap(), "Hello World");

        // Request changes test
        let requeued = store.requeue_task(&task.id).unwrap();
        assert_eq!(requeued.status, TaskStatus::Queued);

        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = std::fs::remove_dir_all(&proj_dir);
    }
}

#[cfg(test)]
mod fanout_tests {
    use super::*;

    #[test]
    fn test_fanout_depth_and_child_caps() {
        let temp_dir = std::env::temp_dir().join(format!("ghostlink_fanout_{}", Uuid::new_v4()));
        let store = TaskRuntimeStore::new(&temp_dir).unwrap();

        let proj_dir =
            std::env::temp_dir().join(format!("ghostlink_fanout_proj_{}", Uuid::new_v4()));
        fs::create_dir_all(&proj_dir).unwrap();

        let proj = store
            .create_project(
                "Fanout Proj".into(),
                ProjectKind::Code,
                proj_dir.to_string_lossy().to_string(),
                None,
                None,
            )
            .unwrap();

        let parent = store
            .create_task(&proj.id, "Parent Goal".into(), None, None, None)
            .unwrap();

        // Create 4 children (max_children=4)
        for i in 0..4 {
            let child = store
                .create_task(
                    &proj.id,
                    format!("Child {}", i),
                    None,
                    None,
                    Some(parent.id.clone()),
                )
                .unwrap();
            assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
        }

        // 5th child fails
        let child5_err = store.create_task(
            &proj.id,
            "Child 5".into(),
            None,
            None,
            Some(parent.id.clone()),
        );
        assert!(child5_err.is_err());
        assert!(child5_err
            .unwrap_err()
            .to_string()
            .contains("max_children=4"));

        // Sub-child fails (max_depth=1)
        let child1 = store
            .list_project_tasks(&proj.id)
            .unwrap()
            .into_iter()
            .find(|t| t.parent_id == Some(parent.id.clone()))
            .unwrap();
        let subchild_err = store.create_task(
            &proj.id,
            "Sub-child".into(),
            None,
            None,
            Some(child1.id.clone()),
        );
        assert!(subchild_err.is_err());
        assert!(subchild_err
            .unwrap_err()
            .to_string()
            .contains("max_depth=1"));

        // Parent accept blocked
        let check_accept = store.check_parent_accept_allowed(&proj.id, &parent.id);
        assert!(check_accept.is_err());

        // Cancel cascade
        let _ = store.cancel_task_run(&parent.id);
        let check_accept_after_cancel = store.check_parent_accept_allowed(&proj.id, &parent.id);
        assert!(check_accept_after_cancel.is_ok());

        let _ = fs::remove_dir_all(&temp_dir);
        let _ = fs::remove_dir_all(&proj_dir);
    }
}

#[cfg(test)]
mod parallel_and_compact_tests {
    use super::*;

    #[tokio::test]
    async fn test_parallel_isolated_tools_execution() {
        let temp_dir = std::env::temp_dir().join(format!("ghostlink_par_{}", Uuid::new_v4()));
        let store = Arc::new(TaskRuntimeStore::new(&temp_dir).unwrap());

        let proj_dir = std::env::temp_dir().join(format!("ghostlink_par_proj_{}", Uuid::new_v4()));
        fs::create_dir_all(&proj_dir).unwrap();
        fs::write(proj_dir.join("file1.txt"), "content 1").unwrap();
        fs::write(proj_dir.join("file2.txt"), "content 2").unwrap();

        let proj = store
            .create_project(
                "Parallel Proj".into(),
                ProjectKind::Code,
                proj_dir.to_string_lossy().to_string(),
                None,
                None,
            )
            .unwrap();

        let task = store
            .create_task(
                &proj.id,
                "Read two files in parallel".into(),
                None,
                None,
                None,
            )
            .unwrap();

        let responses = Arc::new(Mutex::new(vec![
            AgentResponse {
                content: Some("Reading files...".into()),
                tool_calls: vec![
                    ToolCall {
                        id: "call_1".into(),
                        name: "read_file".into(),
                        args: serde_json::json!({ "path": "file1.txt" }),
                    },
                    ToolCall {
                        id: "call_2".into(),
                        name: "read_file".into(),
                        args: serde_json::json!({ "path": "file2.txt" }),
                    },
                ],
            },
            AgentResponse {
                content: Some("Done reading".into()),
                tool_calls: vec![],
            },
        ]));

        let backend = Arc::new(FakeBackend { responses });
        let cancel_token = tokio_util::sync::CancellationToken::new();

        let run = TaskRunner::spawn_implementer_joined(
            store.clone(),
            backend,
            task.clone(),
            "implementer".into(),
            "test-model".into(),
            None,
            cancel_token,
        )
        .await
        .unwrap();

        assert_eq!(run.status, "finished");
        let review = store.get_task_review(&task.id).unwrap();
        assert_eq!(review.task_id, task.id);
    }

    #[tokio::test]
    async fn test_parent_review_waits_for_child_completion() {
        let temp_dir = std::env::temp_dir().join(format!("ghostlink_wait_{}", Uuid::new_v4()));
        let store = Arc::new(TaskRuntimeStore::new(&temp_dir).unwrap());

        let proj_dir = std::env::temp_dir().join(format!("ghostlink_wait_proj_{}", Uuid::new_v4()));
        fs::create_dir_all(&proj_dir).unwrap();

        let proj = store
            .create_project(
                "Wait Proj".into(),
                ProjectKind::Code,
                proj_dir.to_string_lossy().to_string(),
                None,
                None,
            )
            .unwrap();

        let parent_task = store
            .create_task(&proj.id, "Parent task goal".into(), None, None, None)
            .unwrap();

        let responses = Arc::new(Mutex::new(vec![
            AgentResponse {
                content: Some("Spawning child...".into()),
                tool_calls: vec![ToolCall {
                    id: "call_spawn".into(),
                    name: "spawn_subagent".into(),
                    args: serde_json::json!({ "goal": "Child goal" }),
                }],
            },
            AgentResponse {
                content: Some("Parent finished".into()),
                tool_calls: vec![],
            },
            AgentResponse {
                content: Some("Child finished".into()),
                tool_calls: vec![],
            },
        ]));

        let backend = Arc::new(FakeBackend { responses });
        let cancel_token = tokio_util::sync::CancellationToken::new();

        let run = TaskRunner::spawn_implementer_joined(
            store.clone(),
            backend,
            parent_task.clone(),
            "planner".into(),
            "test-model".into(),
            None,
            cancel_token,
        )
        .await
        .unwrap();

        assert_eq!(run.status, "finished");
        let children = store.list_child_tasks(&parent_task.id).unwrap();
        assert_eq!(children.len(), 1);
    }

    #[tokio::test]
    async fn test_compact_review_diff_generation() {
        let temp_dir = std::env::temp_dir().join(format!("ghostlink_diff_{}", Uuid::new_v4()));
        let store = Arc::new(TaskRuntimeStore::new(&temp_dir).unwrap());

        let proj_dir = std::env::temp_dir().join(format!("ghostlink_diff_proj_{}", Uuid::new_v4()));
        fs::create_dir_all(&proj_dir).unwrap();
        fs::write(proj_dir.join("code.rs"), "fn hello() {}\n").unwrap();

        let proj = store
            .create_project(
                "Diff Proj".into(),
                ProjectKind::Code,
                proj_dir.to_string_lossy().to_string(),
                None,
                None,
            )
            .unwrap();

        let task = store
            .create_task(&proj.id, "Update code".into(), None, None, None)
            .unwrap();

        store
            .write_proposed_file(
                &proj.root_path,
                &task.id,
                "code.rs",
                "fn hello() {\n    println!(\"world\");\n}\n",
            )
            .unwrap();

        let responses = Arc::new(Mutex::new(vec![AgentResponse {
            content: Some("Done".into()),
            tool_calls: vec![],
        }]));

        let backend = Arc::new(FakeBackend { responses });
        let cancel_token = tokio_util::sync::CancellationToken::new();

        let _run = TaskRunner::spawn_implementer_joined(
            store.clone(),
            backend,
            task.clone(),
            "implementer".into(),
            "test-model".into(),
            None,
            cancel_token,
        )
        .await
        .unwrap();

        let review = store.get_task_review(&task.id).unwrap();
        assert_eq!(review.diffs.len(), 1);
        assert!(review.diffs[0].unified_diff.contains("--- a/code.rs"));
        assert!(review.diffs[0].unified_diff.contains("+++ b/code.rs"));
    }
}
