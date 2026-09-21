use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::error::Error;

#[derive(Clone, Debug)]
pub struct VllmClient {
    base_url: String,
    api_key: Option<String>,
    client: Client,
}

#[derive(Debug, Deserialize)]
struct ModelListResponse {
    data: Vec<ModelCard>,
}

#[derive(Debug, Deserialize)]
struct ModelCard {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    // `None` (not just empty) whenever the response is tool_calls-only —
    // vLLM's OpenAI-compatible server sends a literal JSON `null` there,
    // which fails to deserialize into a non-Option `String`.
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<VllmToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VllmToolCall {
    #[serde(default)]
    pub id: Option<String>,
    pub function: VllmFunctionCall,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VllmFunctionCall {
    pub name: String,
    /// Raw JSON-encoded string per the OpenAI tool-call shape (not a nested
    /// object) — callers must `serde_json::from_str` this themselves.
    #[serde(default)]
    pub arguments: String,
}

/// Result of a tool-aware chat turn: the model's text (possibly empty when
/// it only requested tools) plus any tool calls it asked for.
pub struct VllmChatResult {
    pub content: String,
    pub tool_calls: Vec<VllmToolCall>,
}

impl VllmClient {
    pub fn new(base_url: String, api_key: Option<String>) -> Self {
        let client = Client::builder()
            .pool_max_idle_per_host(10)
            .tcp_keepalive(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            base_url: normalize_base_url(&base_url),
            api_key: api_key.filter(|key| !key.trim().is_empty()),
            client,
        }
    }

    #[allow(dead_code)]
    pub async fn health(&self) -> Result<bool, Box<dyn Error>> {
        let health_url = format!("{}/health", self.base_url);
        let response = self.request(self.client.get(health_url)).send().await;
        match response {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(_) => {
                let fallback = self
                    .request(self.client.get(format!("{}/v1/models", self.base_url)))
                    .send()
                    .await?;
                Ok(fallback.status().is_success())
            }
        }
    }

    pub async fn list_models(&self) -> Result<Vec<String>, Box<dyn Error>> {
        let resp = self
            .request(self.client.get(format!("{}/v1/models", self.base_url)))
            .send()
            .await?;
        let resp = resp.error_for_status()?;
        let payload: ModelListResponse = resp.json().await?;
        Ok(payload.data.into_iter().map(|model| model.id).collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn generate(
        &self,
        model: &str,
        prompt: &str,
        temperature: f32,
        top_p: f32,
        top_k: usize,
        repeat_penalty: f32,
        max_tokens: usize,
    ) -> Result<String, Box<dyn Error>> {
        let payload = json!({
            "model": model,
            "messages": [{ "role": "user", "content": prompt }],
            "stream": false,
            "temperature": temperature.clamp(0.0, 2.0),
            "top_p": top_p.clamp(0.0, 1.0),
            "top_k": top_k.clamp(1, 200),
            "repetition_penalty": repeat_penalty.clamp(0.0, 2.0),
            "max_tokens": max_tokens,
        });

        let resp = self
            .request(
                self.client
                    .post(format!("{}/v1/chat/completions", self.base_url)),
            )
            .json(&payload)
            .send()
            .await?;
        let resp = resp.error_for_status()?;
        let payload: ChatCompletionResponse = resp.json().await?;
        Ok(payload
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .unwrap_or_default())
    }

    /// Tool- and structured-output-aware chat turn. `messages` is passed
    /// through as raw OpenAI-shaped JSON (system/user/assistant/tool roles)
    /// since a tool follow-up turn needs to echo back the assistant's own
    /// `tool_calls` plus a `tool` role reply per call — a fixed message
    /// struct can't represent that without duplicating this shape anyway.
    #[allow(clippy::too_many_arguments)]
    pub async fn chat(
        &self,
        model: &str,
        messages: Vec<Value>,
        temperature: f32,
        top_p: f32,
        top_k: usize,
        repeat_penalty: f32,
        max_tokens: usize,
        tools: Option<Vec<Value>>,
        response_format: Option<Value>,
    ) -> Result<VllmChatResult, Box<dyn Error>> {
        let mut payload = json!({
            "model": model,
            "messages": messages,
            "stream": false,
            "temperature": temperature.clamp(0.0, 2.0),
            "top_p": top_p.clamp(0.0, 1.0),
            "top_k": top_k.clamp(1, 200),
            "repetition_penalty": repeat_penalty.clamp(0.0, 2.0),
            "max_tokens": max_tokens,
        });

        if let Some(obj) = payload.as_object_mut() {
            if let Some(tools) = tools {
                obj.insert("tools".to_string(), json!(tools));
            }
            if let Some(response_format) = response_format {
                obj.insert("response_format".to_string(), response_format);
            }
        }

        let resp = self
            .request(
                self.client
                    .post(format!("{}/v1/chat/completions", self.base_url)),
            )
            .json(&payload)
            .send()
            .await?;
        let resp = resp.error_for_status()?;
        let payload: ChatCompletionResponse = resp.json().await?;
        let message = payload
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message)
            .unwrap_or(ChatMessage {
                content: None,
                tool_calls: None,
            });

        Ok(VllmChatResult {
            content: message.content.unwrap_or_default(),
            tool_calls: message.tool_calls.unwrap_or_default(),
        })
    }

    /// Streaming chat turn for vLLM over OpenAI /v1/chat/completions endpoint.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    pub async fn chat_stream(
        &self,
        model: &str,
        messages: Vec<Value>,
        temperature: f32,
        top_p: f32,
        top_k: usize,
        repeat_penalty: f32,
        max_tokens: usize,
        response_format: Option<Value>,
    ) -> Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<String, Box<dyn Error + Send + Sync>>> + Send>,
        >,
        Box<dyn Error>,
    > {
        use futures::StreamExt;
        let mut payload = json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "temperature": temperature.clamp(0.0, 2.0),
            "top_p": top_p.clamp(0.0, 1.0),
            "top_k": top_k.clamp(1, 200),
            "repetition_penalty": repeat_penalty.clamp(0.0, 2.0),
            "max_tokens": max_tokens,
        });

        if let Some(obj) = payload.as_object_mut() {
            if let Some(response_format) = response_format {
                obj.insert("response_format".to_string(), response_format);
            }
        }

        let resp = self
            .request(
                self.client
                    .post(format!("{}/v1/chat/completions", self.base_url)),
            )
            .json(&payload)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Box::new(std::io::Error::other(format!(
                "vLLM streaming request failed HTTP {status}: {body}"
            ))));
        }

        let (tx, rx) = tokio::sync::mpsc::channel(100);
        tokio::spawn(async move {
            let mut byte_stream = resp.bytes_stream();
            let mut buf = String::new();
            while let Some(chunk) = byte_stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx
                            .send(Err(Box::new(e) as Box<dyn Error + Send + Sync>))
                            .await;
                        return;
                    }
                };
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(pos) = buf.find('\n') {
                    let line = buf[..pos].trim().to_string();
                    buf.drain(..=pos);
                    if line.is_empty() {
                        continue;
                    }
                    let payload = match line.strip_prefix("data: ") {
                        Some(p) => p,
                        None => continue,
                    };
                    if payload == "[DONE]" {
                        return;
                    }
                    if let Ok(data) = serde_json::from_str::<Value>(payload) {
                        if let Some(delta) = data
                            .get("choices")
                            .and_then(|c| c.get(0))
                            .and_then(|choice| choice.get("delta"))
                        {
                            if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                                if !text.is_empty() && tx.send(Ok(text.to_string())).await.is_err()
                                {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    fn request(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(api_key) = &self.api_key {
            let mut headers = HeaderMap::new();
            if let Ok(value) = HeaderValue::from_str(&format!("Bearer {api_key}")) {
                headers.insert(AUTHORIZATION, value);
            }
            builder.headers(headers)
        } else {
            builder
        }
    }
}

fn normalize_base_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if let Some(stripped) = trimmed.strip_suffix("/v1") {
        stripped.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_base_url;

    #[test]
    fn strips_trailing_v1_segment() {
        assert_eq!(
            normalize_base_url("http://127.0.0.1:8000/v1/"),
            "http://127.0.0.1:8000"
        );
        assert_eq!(
            normalize_base_url("http://127.0.0.1:8000"),
            "http://127.0.0.1:8000"
        );
    }
}
