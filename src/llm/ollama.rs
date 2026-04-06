//! Ollama HTTP client — calls the real LLM.
//!
//! This is the part everyone else skips. A real HTTP client that sends
//! a real prompt to a real model and returns real text.

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use super::{ChatMessage, ChatRequest, ChatResponse, CompletionStats, LlmClient, LlmError};

/// Ollama API client.
pub struct OllamaClient {
    base_url: String,
    client: Client,
}

// -- Ollama API request/response types --

#[derive(Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaOptions>,
}

#[derive(Serialize)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<i32>,
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

#[derive(Serialize, Deserialize)]
struct OllamaMessage {
    role: String,
    content: String,
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

        let body = OllamaChatRequest {
            model: req.model.clone(),
            messages: req.messages,
            stream: false, // Always non-streaming for now
            options,
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
            message: ChatMessage {
                role: ollama_resp.message.role,
                content: ollama_resp.message.content,
            },
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
