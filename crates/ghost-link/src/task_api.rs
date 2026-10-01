use crate::backend_plugin;
use crate::ollama::ChatMessage as OllamaChatMessage;
use crate::task_runtime::{
    AgentBackend, AgentResponse, ProjectKind, TaskBudget, TaskStatus, ToolCall,
};
use crate::BackendState;
use crate::InferenceEngine;
use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, Sse},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

pub struct RealAgentBackend {
    pub state: Arc<std::sync::Mutex<BackendState>>,
}

pub fn build_tool_definitions(allowed_tools: &[String]) -> Vec<Value> {
    let mut tools = Vec::new();

    if allowed_tools.contains(&"write_file".to_string()) {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Write or overwrite a file in the project proposed staging directory.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative file path within project" },
                        "content": { "type": "string", "description": "File contents" }
                    },
                    "required": ["path", "content"]
                }
            }
        }));
    }

    if allowed_tools.contains(&"read_file".to_string()) {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read file contents from proposed staging or live workspace.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative file path within project" }
                    },
                    "required": ["path"]
                }
            }
        }));
    }

    if allowed_tools.contains(&"run_command".to_string())
        || allowed_tools.contains(&"execute".to_string())
    {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "Execute a shell command evaluated through the deterministic Judge security policy.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "argv": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Command and arguments as array of strings"
                        }
                    },
                    "required": ["argv"]
                }
            }
        }));
    }

    if allowed_tools.contains(&"create_child_task".to_string())
        || allowed_tools.contains(&"spawn_subagent".to_string())
    {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "create_child_task",
                "description": "Spawn a child subtask for a sub-goal (planner only).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "goal": { "type": "string", "description": "Goal for the child task" },
                        "acceptance_criteria": { "type": "string", "description": "Optional acceptance criteria" }
                    },
                    "required": ["goal"]
                }
            }
        }));
    }

    tools
}

pub fn parse_fallback_tool_calls(text: &str) -> Vec<ToolCall> {
    let mut tool_calls = Vec::new();

    let try_parse = |json_val: &Value, tcs: &mut Vec<ToolCall>| {
        if let Some(arr) = json_val.get("tool_calls").and_then(|tc| tc.as_array()) {
            for item in arr {
                if let (Some(name), Some(args)) =
                    (item.get("name").and_then(|n| n.as_str()), item.get("args"))
                {
                    tcs.push(ToolCall {
                        id: format!("tc_{}", uuid::Uuid::new_v4().simple()),
                        name: name.to_string(),
                        args: args.clone(),
                    });
                }
            }
        }
    };

    if let Some(start) = text.find("{\"tool_calls\":") {
        if let Some(end) = text[start..].rfind("]}") {
            let candidate = &text[start..start + end + 2];
            if let Ok(val) = serde_json::from_str::<Value>(candidate) {
                try_parse(&val, &mut tool_calls);
            }
        }
    }

    if tool_calls.is_empty() {
        if let Ok(val) = serde_json::from_str::<Value>(text.trim()) {
            try_parse(&val, &mut tool_calls);
        }
    }

    tool_calls
}

#[async_trait::async_trait]
impl AgentBackend for RealAgentBackend {
    async fn chat(&self, messages: &[Value], allowed_tools: &[String]) -> Result<AgentResponse> {
        let (
            model,
            native_engine_client,
            ollama_client,
            vllm_client,
            settings,
            plugin_registry,
            inference_backend,
        ) = {
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

        let tool_defs = build_tool_definitions(allowed_tools);
        let tools_option = if !tool_defs.is_empty() {
            Some(tool_defs)
        } else {
            None
        };

        if let Some(plugin) = plugin_registry.get(&settings.inference_backend) {
            let mut prompt = String::new();
            for msg in messages {
                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                if !prompt.is_empty() {
                    prompt.push('\n');
                }
                prompt.push_str(role);
                prompt.push_str(": ");
                prompt.push_str(content);
            }

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

            let tool_calls = parse_fallback_tool_calls(&res.text);

            return Ok(AgentResponse {
                content: Some(res.text),
                tool_calls,
            });
        }

        match inference_backend {
            InferenceEngine::Ollama => {
                let ollama_msgs: Vec<OllamaChatMessage> = messages
                    .iter()
                    .filter_map(|m| serde_json::from_value(m.clone()).ok())
                    .collect();

                let res = ollama_client
                    .chat(
                        &model,
                        &ollama_msgs,
                        Some(0.7),
                        Some(0.9),
                        Some(40),
                        Some(1.1),
                        Some(1024),
                        tools_option.clone(),
                        None,
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;

                let mut tool_calls = Vec::new();
                if let Some(raw_tcs) = res.message.tool_calls {
                    for tc in raw_tcs {
                        if let Some(func) = tc.get("function") {
                            if let (Some(name), Some(args)) = (
                                func.get("name").and_then(|n| n.as_str()),
                                func.get("arguments"),
                            ) {
                                tool_calls.push(ToolCall {
                                    id: format!("tc_{}", uuid::Uuid::new_v4().simple()),
                                    name: name.to_string(),
                                    args: args.clone(),
                                });
                            }
                        }
                    }
                }

                if tool_calls.is_empty() && !res.message.content.is_empty() {
                    tool_calls = parse_fallback_tool_calls(&res.message.content);
                }

                Ok(AgentResponse {
                    content: Some(res.message.content),
                    tool_calls,
                })
            }
            InferenceEngine::Vllm => {
                let msgs_vec = messages.to_vec();
                let res = vllm_client
                    .chat(
                        &model,
                        msgs_vec,
                        0.7,
                        0.9,
                        40,
                        1.1,
                        1024,
                        tools_option.clone(),
                        None,
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))?;

                let mut tool_calls = Vec::new();
                for tc in res.tool_calls {
                    if let Ok(args_val) = serde_json::from_str::<Value>(&tc.function.arguments) {
                        tool_calls.push(ToolCall {
                            id: tc
                                .id
                                .unwrap_or_else(|| format!("tc_{}", uuid::Uuid::new_v4().simple())),
                            name: tc.function.name,
                            args: args_val,
                        });
                    }
                }

                if tool_calls.is_empty() && !res.content.is_empty() {
                    tool_calls = parse_fallback_tool_calls(&res.content);
                }

                Ok(AgentResponse {
                    content: Some(res.content),
                    tool_calls,
                })
            }
            InferenceEngine::Native => {
                let mut prompt = String::new();
                for msg in messages {
                    let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                    let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    if !prompt.is_empty() {
                        prompt.push('\n');
                    }
                    prompt.push_str(role);
                    prompt.push_str(": ");
                    prompt.push_str(content);
                }

                if let Some(ref tool_defs) = tools_option {
                    prompt.push_str("\n\nAvailable tools (if tool calls are required, respond with {\"tool_calls\": [{\"name\": \"...\", \"args\": {...}}]}):\n");
                    if let Ok(tools_json) = serde_json::to_string_pretty(tool_defs) {
                        prompt.push_str(&tools_json);
                    }
                }

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

                let tool_calls = parse_fallback_tool_calls(&gen.text);

                Ok(AgentResponse {
                    content: Some(gen.text),
                    tool_calls,
                })
            }
        }
    }
}

#[derive(Deserialize)]
pub struct CreateProjectReq {
    pub name: String,
    pub kind: Option<ProjectKind>,
    pub root_path: String,
    pub allowed_tools: Option<Vec<String>>,
    pub default_model: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateProjectReq {
    pub name: Option<String>,
    pub default_model: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
}

async fn handle_create_project(
    State(state): State<Arc<std::sync::Mutex<BackendState>>>,
    Json(payload): Json<CreateProjectReq>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let store = Arc::clone(&state.lock().unwrap().task_store);
    let kind = payload.kind.unwrap_or(ProjectKind::Code);
    match store.create_project(
        payload.name,
        kind,
        payload.root_path,
        payload.allowed_tools,
        payload.default_model,
    ) {
        Ok(proj) => Ok((
            StatusCode::CREATED,
            Json(serde_json::to_value(proj).unwrap()),
        )),
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )),
    }
}

async fn handle_list_projects(
    State(state): State<Arc<std::sync::Mutex<BackendState>>>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let store = Arc::clone(&state.lock().unwrap().task_store);
    match store.list_projects() {
        Ok(projs) => Ok(Json(serde_json::to_value(projs).unwrap())),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )),
    }
}

async fn handle_get_project(
    State(state): State<Arc<std::sync::Mutex<BackendState>>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let store = Arc::clone(&state.lock().unwrap().task_store);
    match store.get_project(&id) {
        Ok(proj) => Ok(Json(serde_json::to_value(proj).unwrap())),
        Err(e) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )),
    }
}

async fn handle_update_project(
    State(state): State<Arc<std::sync::Mutex<BackendState>>>,
    Path(id): Path<String>,
    Json(payload): Json<UpdateProjectReq>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let store = Arc::clone(&state.lock().unwrap().task_store);
    match store.update_project(
        &id,
        payload.name,
        payload.default_model,
        payload.allowed_tools,
    ) {
        Ok(proj) => Ok(Json(serde_json::to_value(proj).unwrap())),
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )),
    }
}

#[derive(Deserialize)]
pub struct CreateTaskReq {
    pub goal: String,
    pub acceptance_criteria: Option<String>,
    pub budget: Option<TaskBudget>,
    pub parent_id: Option<String>,
}

async fn handle_create_project_task(
    State(state): State<Arc<std::sync::Mutex<BackendState>>>,
    Path(project_id): Path<String>,
    Json(payload): Json<CreateTaskReq>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let store = Arc::clone(&state.lock().unwrap().task_store);
    match store.create_task(
        &project_id,
        payload.goal,
        payload.acceptance_criteria,
        payload.budget,
        payload.parent_id,
    ) {
        Ok(task) => Ok((
            StatusCode::CREATED,
            Json(serde_json::to_value(task).unwrap()),
        )),
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )),
    }
}

async fn handle_list_project_tasks(
    State(state): State<Arc<std::sync::Mutex<BackendState>>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let store = Arc::clone(&state.lock().unwrap().task_store);
    match store.list_project_tasks(&project_id) {
        Ok(tasks) => Ok(Json(serde_json::to_value(tasks).unwrap())),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )),
    }
}

async fn handle_get_task(
    State(state): State<Arc<std::sync::Mutex<BackendState>>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let store = Arc::clone(&state.lock().unwrap().task_store);
    match store.get_task(&id) {
        Ok(task) => Ok(Json(serde_json::to_value(task).unwrap())),
        Err(e) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )),
    }
}

#[derive(Deserialize)]
pub struct SpawnTaskReq {
    pub role: Option<String>,
    pub model: Option<String>,
    pub brief: Option<String>,
}

async fn handle_spawn_task(
    State(state): State<Arc<std::sync::Mutex<BackendState>>>,
    Path(id): Path<String>,
    Json(payload): Json<SpawnTaskReq>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let (store, current_model) = {
        let guard = state.lock().unwrap();
        (Arc::clone(&guard.task_store), guard.current_model.clone())
    };
    let task = match store.get_task(&id) {
        Ok(t) => t,
        Err(e) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": e.to_string() })),
            ))
        }
    };
    let project = match store.get_project(&task.project_id) {
        Ok(p) => p,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            ))
        }
    };

    let role = payload.role.unwrap_or_else(|| "implementer".into());
    let model = payload
        .model
        .or(project.default_model)
        .unwrap_or(current_model);

    let cancel_token = CancellationToken::new();
    store.register_cancel_token(task.id.clone(), cancel_token.clone());

    let backend = Arc::new(RealAgentBackend {
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
    )
    .await
    {
        Ok(run) => Ok((
            StatusCode::ACCEPTED,
            Json(serde_json::to_value(run).unwrap()),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )),
    }
}

async fn handle_cancel_task(
    State(state): State<Arc<std::sync::Mutex<BackendState>>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let store = Arc::clone(&state.lock().unwrap().task_store);
    let _ = store.cancel_task_run(&id);
    match store.update_task_status(&id, TaskStatus::Cancelled) {
        Ok(task) => Ok(Json(serde_json::to_value(task).unwrap())),
        Err(e) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )),
    }
}

async fn handle_get_task_review(
    State(state): State<Arc<std::sync::Mutex<BackendState>>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let store = Arc::clone(&state.lock().unwrap().task_store);
    match store.get_task_review(&id) {
        Ok(review) => Ok(Json(serde_json::to_value(review).unwrap())),
        Err(e) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )),
    }
}

#[derive(Deserialize)]
pub struct DecideReviewReq {
    pub decision: String,
    pub note: Option<String>,
}

async fn handle_decide_review(
    State(state): State<Arc<std::sync::Mutex<BackendState>>>,
    Path(review_id): Path<String>,
    Json(payload): Json<DecideReviewReq>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let store = Arc::clone(&state.lock().unwrap().task_store);
    let review = match store.get_review(&review_id) {
        Ok(r) => r,
        Err(e) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": e.to_string() })),
            ))
        }
    };
    let task = match store.get_task(&review.task_id) {
        Ok(t) => t,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            ))
        }
    };
    let project = match store.get_project(&task.project_id) {
        Ok(p) => p,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            ))
        }
    };

    match payload.decision.as_str() {
        "accept" => {
            if let Err(e) = store.check_parent_accept_allowed(&project.id, &task.id) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": e.to_string() })),
                ));
            }
            let _ = store.apply_proposed_changes(&project.root_path, &task.id);
            let _ = store.update_task_status(&task.id, TaskStatus::Accepted);
            Ok(Json(
                serde_json::json!({ "status": "accepted", "task_id": task.id }),
            ))
        }
        "reject" => {
            let _ = store.discard_proposed_changes(&project.root_path, &task.id);
            let _ = store.update_task_status(&task.id, TaskStatus::Rejected);
            Ok(Json(
                serde_json::json!({ "status": "rejected", "task_id": task.id }),
            ))
        }
        "request_changes" => {
            let _ = store.requeue_task(&task.id);
            Ok(Json(
                serde_json::json!({ "status": "queued", "task_id": task.id, "note": payload.note }),
            ))
        }
        _ => Err((
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({ "error": "invalid decision: must be accept, reject, or request_changes" }),
            ),
        )),
    }
}

async fn handle_get_task_events(
    State(state): State<Arc<std::sync::Mutex<BackendState>>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let store = Arc::clone(&state.lock().unwrap().task_store);
    let historical_events = store.get_task_events(&id);
    let rx = store.subscribe_events();

    let (tx, rx_stream) =
        tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(128);

    tokio::spawn(async move {
        // Replay historical events from JSONL
        for event in historical_events {
            if let Ok(json) = serde_json::to_string(&event) {
                let sse_ev = Event::default()
                    .id(format!("{}_{}", event.task_id, event.ts))
                    .data(json);
                if tx.send(Ok(sse_ev)).await.is_err() {
                    return;
                }
            }
        }

        // Stream live tail events from broadcast
        let mut bcast_stream = tokio_stream::wrappers::BroadcastStream::new(rx);
        while let Some(msg) = bcast_stream.next().await {
            if let Ok(event) = msg {
                if event.task_id == id {
                    if let Ok(json) = serde_json::to_string(&event) {
                        let sse_ev = Event::default()
                            .id(format!("{}_{}", event.task_id, event.ts))
                            .data(json);
                        if tx.send(Ok(sse_ev)).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx_stream);
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

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

pub fn router() -> Router<Arc<std::sync::Mutex<BackendState>>> {
    Router::new()
        .route(
            "/api/projects",
            post(handle_create_project).get(handle_list_projects),
        )
        .route(
            "/api/projects/:id",
            get(handle_get_project).patch(handle_update_project),
        )
        .route(
            "/api/projects/:id/tasks",
            post(handle_create_project_task).get(handle_list_project_tasks),
        )
        .route("/api/tasks/:id", get(handle_get_task))
        .route("/api/tasks/:id/children", get(handle_get_task_children))
        .route("/api/tasks/:id/spawn", post(handle_spawn_task))
        .route("/api/tasks/:id/cancel", post(handle_cancel_task))
        .route("/api/tasks/:id/events", get(handle_get_task_events))
        .route("/api/tasks/:id/review", get(handle_get_task_review))
        .route("/api/reviews/:id/decide", post(handle_decide_review))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_fallback_tool_calls_single_and_multiple() {
        let text_single =
            r#"{"tool_calls":[{"name":"write_file","args":{"path":"a.txt","content":"hello"}}]}"#;
        let tcs = parse_fallback_tool_calls(text_single);
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].name, "write_file");
        assert_eq!(tcs[0].args["path"], "a.txt");

        let text_multi = r#"Prose before... {"tool_calls":[{"name":"read_file","args":{"path":"b.txt"}},{"name":"run_command","args":{"argv":["ls"]}}]} prose after"#;
        let tcs_multi = parse_fallback_tool_calls(text_multi);
        assert_eq!(tcs_multi.len(), 2);
        assert_eq!(tcs_multi[0].name, "read_file");
        assert_eq!(tcs_multi[1].name, "run_command");
    }

    #[test]
    fn test_parse_fallback_tool_calls_rejects_single_object() {
        let single_obj = r#"{"tool_call":{"name":"write_file","args":{"path":"a.txt"}}}"#;
        let tcs = parse_fallback_tool_calls(single_obj);
        assert!(
            tcs.is_empty(),
            "Single tool_call object should be rejected in favor of tool_calls array"
        );
    }

    #[test]
    fn test_build_tool_definitions_filters_by_allowed_tools() {
        let allowed = vec!["write_file".to_string(), "read_file".to_string()];
        let defs = build_tool_definitions(&allowed);
        assert_eq!(defs.len(), 2);
        let names: Vec<&str> = defs
            .iter()
            .filter_map(|d| d.get("function")?.get("name")?.as_str())
            .collect();
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"read_file"));
        assert!(!names.contains(&"run_command"));
        assert!(!names.contains(&"create_child_task"));
    }
}
