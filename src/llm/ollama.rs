//! Ollama HTTP client — calls the real LLM.
//!
//! This is the part everyone else skips. A real HTTP client that sends
//! a real prompt to a real model and returns real text.
//!
//! ## Tool calling
//!
//! When `ChatRequest.tools` is `Some`, the request includes tool definitions
//! and the response may contain `tool_calls` in the assistant message instead
//! of (or alongside) text content. The caller drives the tool loop — this
//! client is stateless and handles a single request/response round.
//!
//! Ollama wire format for tools:
//! - Request: `"tools": [{ "type": "function", "function": { ... } }]`
//! - Response: `"message": { "role": "assistant", "tool_calls": [{ "function": { "name": "...", "arguments": {...} } }] }`
//! - Follow-up: `{ "role": "tool", "content": "...", "tool_name": "..." }` (not handled here — caller builds messages)

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use super::{
    ChatMessage, ChatRequest, ChatResponse, CompletionStats, LlmClient, LlmError, ToolCall,
    ToolCallFunction, ToolDefinition,
};

/// Ollama API client.
pub struct OllamaClient {
    base_url: String,
    client: Client,
}

// -- Ollama API request/response types --

#[derive(Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
}

#[derive(Serialize)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<i32>,
}

/// Ollama message wire type — supports both plain text and tool-related messages.
///
/// Separate from `ChatMessage` because Ollama's JSON shape differs slightly
/// (e.g. `tool_calls` can be absent or present, `tool_name` on tool results).
#[derive(Serialize, Deserialize, Debug)]
struct OllamaMessage {
    role: String,
    #[serde(default)]
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OllamaToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
}

/// Ollama's tool call wire format.
#[derive(Serialize, Deserialize, Debug)]
struct OllamaToolCall {
    #[serde(rename = "type", default = "default_function")]
    call_type: String,
    function: OllamaToolCallFunction,
}

fn default_function() -> String {
    "function".into()
}

#[derive(Serialize, Deserialize, Debug)]
struct OllamaToolCallFunction {
    name: String,
    arguments: serde_json::Value,
    /// Ollama includes an `index` field for parallel tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index: Option<i32>,
}

#[derive(Deserialize)]
struct OllamaChatResponse {
    model: String,
    message: OllamaMessage,
    done: bool,
    #[serde(default)]
    total_duration: Option<i64>,
    #[serde(default)]
    prompt_eval_count: Option<i32>,
    #[serde(default)]
    eval_count: Option<i32>,
}

#[derive(Serialize)]
struct OllamaEmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Deserialize)]
struct OllamaEmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Deserialize)]
struct OllamaModelList {
    models: Vec<OllamaModelInfo>,
}

#[derive(Deserialize)]
struct OllamaModelInfo {
    name: String,
}

// -- Conversions between internal and Ollama wire types --

impl From<&ChatMessage> for OllamaMessage {
    fn from(msg: &ChatMessage) -> Self {
        Self {
            role: msg.role.clone(),
            content: msg.content.clone(),
            tool_calls: msg.tool_calls.as_ref().map(|tcs| {
                tcs.iter()
                    .enumerate()
                    .map(|(i, tc)| OllamaToolCall {
                        call_type: "function".into(),
                        function: OllamaToolCallFunction {
                            name: tc.function.name.clone(),
                            arguments: tc.function.arguments.clone(),
                            index: Some(i as i32),
                        },
                    })
                    .collect()
            }),
            tool_name: msg.tool_name.clone(),
        }
    }
}

impl From<OllamaMessage> for ChatMessage {
    fn from(msg: OllamaMessage) -> Self {
        let tool_calls = msg.tool_calls.map(|tcs| {
            tcs.into_iter()
                .map(|tc| ToolCall {
                    call_type: tc.call_type,
                    function: ToolCallFunction {
                        name: tc.function.name,
                        arguments: tc.function.arguments,
                    },
                })
                .collect()
        });
        Self {
            role: msg.role,
            content: msg.content,
            tool_calls,
            tool_name: msg.tool_name,
        }
    }
}

impl OllamaClient {
    pub fn new(base_url: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(600)) // LLM calls can be slow
            .build()
            .expect("failed to build HTTP client");

        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
        }
    }
}

#[async_trait]
impl LlmClient for OllamaClient {
    async fn chat(&self, req: ChatRequest) -> Result<(ChatResponse, CompletionStats), LlmError> {
        let url = format!("{}/api/chat", self.base_url);
        let start = Instant::now();

        let options = if req.temperature.is_some() || req.max_tokens.is_some() {
            Some(OllamaOptions {
                temperature: req.temperature,
                num_predict: req.max_tokens,
            })
        } else {
            None
        };

        // Convert tools — pass None if empty to avoid sending an empty array
        let tools = req
            .tools
            .filter(|t| !t.is_empty());

        let body = OllamaChatRequest {
            model: req.model.clone(),
            messages: req.messages.iter().map(OllamaMessage::from).collect(),
            stream: false, // Always non-streaming for now
            options,
            tools,
        };

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    LlmError::Unreachable {
                        url: url.clone(),
                        source: e,
                    }
                } else {
                    LlmError::Http(e)
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(LlmError::ProviderError {
                status: status.as_u16(),
                body: body_text,
            });
        }

        let ollama_resp: OllamaChatResponse = response.json().await?;
        let elapsed = start.elapsed();

        let chat_response = ChatResponse {
            message: ChatMessage::from(ollama_resp.message),
            model: ollama_resp.model,
            done: ollama_resp.done,
            total_duration_ns: ollama_resp.total_duration,
            prompt_eval_count: ollama_resp.prompt_eval_count,
            eval_count: ollama_resp.eval_count,
        };

        let stats = CompletionStats {
            prompt_tokens: ollama_resp.prompt_eval_count,
            completion_tokens: ollama_resp.eval_count,
            duration_ms: Some(elapsed.as_millis() as i64),
        };

        Ok((chat_response, stats))
    }

    async fn embed(&self, model: &str, input: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        let url = format!("{}/api/embed", self.base_url);

        let body = OllamaEmbedRequest {
            model: model.to_string(),
            input: input.to_vec(),
        };

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    LlmError::Unreachable {
                        url: url.clone(),
                        source: e,
                    }
                } else {
                    LlmError::Http(e)
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(LlmError::ProviderError {
                status: status.as_u16(),
                body: body_text,
            });
        }

        let embed_resp: OllamaEmbedResponse = response.json().await?;
        Ok(embed_resp.embeddings)
    }

    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        let url = format!("{}/api/tags", self.base_url);

        let response = self.client.get(&url).send().await.map_err(|e| {
            if e.is_connect() {
                LlmError::Unreachable {
                    url: url.clone(),
                    source: e,
                }
            } else {
                LlmError::Http(e)
            }
        })?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(LlmError::ProviderError {
                status: status.as_u16(),
                body: body_text,
            });
        }

        let model_list: OllamaModelList = response.json().await?;
        Ok(model_list.models.into_iter().map(|m| m.name).collect())
    }

    async fn ping(&self) -> Result<(), LlmError> {
        let url = format!("{}/api/version", self.base_url);
        self.client
            .get(&url)
            .send()
            .await
            .map_err(|e| LlmError::Unreachable { url, source: e })?;
        Ok(())
    }

    fn provider_name(&self) -> &str {
        "ollama"
    }
}
