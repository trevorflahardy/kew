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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_messages_request_serialization() {
        let req = MessagesRequest {
            model: "claude-sonnet-4-20250514".into(),
            max_tokens: 4096,
            messages: vec![ApiMessage {
                role: "user".into(),
                content: "Hello".into(),
            }],
            system: Some("Be helpful".into()),
            temperature: Some(0.3),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "claude-sonnet-4-20250514");
        assert_eq!(json["max_tokens"], 4096);
        assert_eq!(json["system"], "Be helpful");
        assert_eq!(json["temperature"], 0.3);
        assert_eq!(json["messages"][0]["role"], "user");
    }

    #[test]
    fn test_messages_request_skips_none_fields() {
        let req = MessagesRequest {
            model: "claude-sonnet-4-20250514".into(),
            max_tokens: 1024,
            messages: vec![],
            system: None,
            temperature: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("system").is_none());
        assert!(json.get("temperature").is_none());
    }

    #[test]
    fn test_messages_response_deserialization() {
        let json = r#"{
            "content": [
                {"type": "text", "text": "Hello!"},
                {"type": "text", "text": " How are you?"}
            ],
            "model": "claude-sonnet-4-20250514",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }"#;
        let resp: MessagesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.model, "claude-sonnet-4-20250514");
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.output_tokens, 5);
        assert_eq!(resp.content.len(), 2);

        // Verify text extraction logic
        let text: String = resp
            .content
            .iter()
            .filter_map(|b| b.text.as_deref())
            .collect();
        assert_eq!(text, "Hello! How are you?");
    }

    #[test]
    fn test_api_error_deserialization() {
        let json = r#"{"error": {"message": "Invalid API key"}}"#;
        let err: ApiError = serde_json::from_str(json).unwrap();
        assert_eq!(err.error.message, "Invalid API key");
    }

    #[test]
    fn test_system_prompt_extraction() {
        // Simulate what chat() does: extract system from messages
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: "Be helpful".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Hello".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Another msg".into(),
            },
        ];

        let mut system_prompt = None;
        let mut api_messages = Vec::new();
        for msg in &messages {
            if msg.role == "system" {
                system_prompt = Some(msg.content.clone());
            } else {
                api_messages.push(ApiMessage {
                    role: msg.role.clone(),
                    content: msg.content.clone(),
                });
            }
        }

        assert_eq!(system_prompt, Some("Be helpful".into()));
        assert_eq!(api_messages.len(), 2);
        assert_eq!(api_messages[0].content, "Hello");
    }

    #[test]
    fn test_provider_name() {
        let client = ClaudeClient::new("test-key");
        assert_eq!(client.provider_name(), "claude");
    }

    #[tokio::test]
    async fn test_embed_returns_error() {
        let client = ClaudeClient::new("test-key");
        let result = client.embed("nomic-embed-text", &["test".into()]).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("does not support embeddings"));
    }

    #[tokio::test]
    async fn test_list_models() {
        let client = ClaudeClient::new("test-key");
        let models = client.list_models().await.unwrap();
        assert!(models.len() >= 3);
        assert!(models.iter().any(|m| m.contains("sonnet")));
    }
}
