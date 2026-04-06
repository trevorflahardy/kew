//! Claude API client — Anthropic Messages API.
//!
//! Implements the LlmClient trait for Claude models via the Anthropic API.

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Instant;

use super::{ChatMessage, ChatRequest, ChatResponse, CompletionStats, LlmClient, LlmError};

pub struct ClaudeClient {
    client: Client,
    api_key: String,
    base_url: String,
}

impl ClaudeClient {
    pub fn new(api_key: &str) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .expect("failed to build HTTP client"),
            api_key: api_key.to_string(),
            base_url: "https://api.anthropic.com".to_string(),
        }
    }
}

// --- Anthropic API types ---

#[derive(Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: i32,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
    model: String,
    usage: Usage,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    _type: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: i32,
    output_tokens: i32,
}

#[derive(Deserialize)]
struct ApiError {
    error: ApiErrorDetail,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    message: String,
}

#[async_trait]
impl LlmClient for ClaudeClient {
    async fn chat(&self, req: ChatRequest) -> Result<(ChatResponse, CompletionStats), LlmError> {
        let start = Instant::now();

        // Extract system prompt from messages (Claude API takes it as a top-level field)
        let mut system_prompt = None;
        let mut api_messages = Vec::new();

        for msg in &req.messages {
            if msg.role == "system" {
                system_prompt = Some(msg.content.clone());
            } else {
                api_messages.push(ApiMessage {
                    role: msg.role.clone(),
                    content: msg.content.clone(),
                });
            }
        }

        let body = MessagesRequest {
            model: req.model,
            max_tokens: req.max_tokens.unwrap_or(4096),
            messages: api_messages,
            system: system_prompt,
            temperature: req.temperature,
        };

        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() || e.is_timeout() {
                    LlmError::Unreachable {
                        url: self.base_url.clone(),
                        source: e,
                    }
                } else {
                    LlmError::Http(e)
                }
            })?;

        let status = response.status().as_u16();
        if status != 200 {
            let body_text = response.text().await.unwrap_or_default();
            // Try to parse structured error
            if let Ok(api_err) = serde_json::from_str::<ApiError>(&body_text) {
                return Err(LlmError::ProviderError {
                    status,
                    body: api_err.error.message,
                });
            }
            return Err(LlmError::ProviderError {
                status,
                body: body_text,
            });
        }

        let msg_response: MessagesResponse = response.json().await.map_err(LlmError::Http)?;
        let duration_ms = start.elapsed().as_millis() as i64;

        // Extract text from content blocks
        let text = msg_response
            .content
            .iter()
            .filter_map(|b| b.text.as_deref())
            .collect::<Vec<_>>()
            .join("");

        let chat_response = ChatResponse {
            message: ChatMessage {
                role: "assistant".into(),
                content: text,
            },
            model: msg_response.model,
            done: true,
            total_duration_ns: Some(duration_ms * 1_000_000),
            prompt_eval_count: Some(msg_response.usage.input_tokens),
            eval_count: Some(msg_response.usage.output_tokens),
        };

        let stats = CompletionStats {
            prompt_tokens: Some(msg_response.usage.input_tokens),
            completion_tokens: Some(msg_response.usage.output_tokens),
            duration_ms: Some(duration_ms),
        };

        Ok((chat_response, stats))
    }

    async fn embed(&self, _model: &str, _input: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        // Claude API doesn't have an embedding endpoint
        Err(LlmError::ModelNotFound(
            "Claude API does not support embeddings. Use Ollama with nomic-embed-text.".into(),
        ))
    }

    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        // Return well-known Claude models
        Ok(vec![
            "claude-sonnet-4-20250514".into(),
            "claude-haiku-4-5-20251001".into(),
            "claude-opus-4-20250514".into(),
        ])
    }

    async fn ping(&self) -> Result<(), LlmError> {
        // Quick check: hit the messages endpoint with minimal payload
        // to see if the API key is valid
        let response = self
            .client
            .get(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .map_err(|e| LlmError::Unreachable {
                url: self.base_url.clone(),
                source: e,
            })?;

        // Any non-connection error means the API is reachable
        // (405 Method Not Allowed is expected for GET on /messages)
        let status = response.status().as_u16();
        if status == 401 {
            return Err(LlmError::ProviderError {
                status,
                body: "Invalid API key".into(),
            });
        }
        Ok(())
    }

    fn provider_name(&self) -> &str {
        "claude"
    }
}
