//! LLM client layer: trait definition and provider implementations.
//!
//! Defines the `LlmClient` trait that all providers (Ollama, Claude API)
//! implement. The router maps model names to the correct provider.
//!
//! ## Tool calling
//!
//! The types here support tool-calling (function-calling) across providers.
//! Each provider translates to/from its native wire format internally:
//!
//! - **Ollama** — `tools` array in request, `tool_calls` in assistant message,
//!   `role: "tool"` with `tool_name` for results.
//! - **Claude** — `tools` with `input_schema`, `tool_use` content blocks in
//!   response, `tool_result` content blocks for results.
//!
//! The internal types (`ToolDefinition`, `ToolCall`) use a provider-neutral
//! format. Provider implementations handle translation.

pub mod claude;
pub mod ollama;
pub mod router;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LlmError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("provider not reachable at {url}: {source}")]
    Unreachable { url: String, source: reqwest::Error },
    #[error("model not found: {0}")]
    ModelNotFound(String),
    #[error("provider error ({status}): {body}")]
    ProviderError { status: u16, body: String },
    #[error("timeout after {0:?}")]
    Timeout(std::time::Duration),
}

// --- Chat message types ---

/// A single message in a chat conversation.
///
/// Roles: `"system"`, `"user"`, `"assistant"`, `"tool"`.
///
/// - For assistant messages that request tool calls, `tool_calls` is `Some(...)`.
/// - For tool result messages (`role = "tool"`), `tool_name` identifies which tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// Tool calls requested by the assistant. Present only when `role = "assistant"`
    /// and the model wants to invoke tools before producing a final answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Name of the tool this message provides results for. Present only when
    /// `role = "tool"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl ChatMessage {
    /// Create a plain text message (no tool calls).
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_calls: None,
            tool_name: None,
        }
    }

    /// Create a tool result message sent back to the model after executing a tool.
    pub fn tool_result(tool_name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: content.into(),
            tool_calls: None,
            tool_name: Some(tool_name.into()),
        }
    }

    /// Returns `true` if this assistant message contains tool call requests.
    pub fn has_tool_calls(&self) -> bool {
        self.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty())
    }
}

// --- Tool definitions (sent to the LLM) ---

/// A tool the model can call, sent as part of the request.
///
/// Wire format follows Ollama's convention:
/// ```json
/// { "type": "function", "function": { "name": "...", "description": "...", "parameters": {...} } }
/// ```
/// Providers that use a different format (e.g. Claude) translate internally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ToolFunction,
}

/// The function metadata within a tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the function's parameters.
    pub parameters: serde_json::Value,
}

// --- Tool calls (returned by the LLM) ---

/// A tool invocation requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(rename = "type", default = "default_function_type")]
    pub call_type: String,
    pub function: ToolCallFunction,
}

fn default_function_type() -> String {
    "function".into()
}

/// The function name and arguments within a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    /// Arguments as a JSON object. The agent tool executor deserialises these
    /// into the appropriate parameter struct.
    pub arguments: serde_json::Value,
}

// --- Chat request/response ---

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i32>,
    /// Tool definitions the model may call. If `None` or empty, the model
    /// produces a plain text response (no tool loop).
    pub tools: Option<Vec<ToolDefinition>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub message: ChatMessage,
    pub model: String,
    pub done: bool,
    pub total_duration_ns: Option<i64>,
    pub prompt_eval_count: Option<i32>,
    pub eval_count: Option<i32>,
}

/// Execution stats returned alongside the response text.
#[derive(Debug, Clone, Default)]
pub struct CompletionStats {
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub duration_ms: Option<i64>,
}

/// The core trait every LLM provider implements.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Send a chat completion request and return the full response.
    ///
    /// If `req.tools` is `Some`, the provider includes tool definitions in the
    /// request and the response may contain `tool_calls` in the assistant message.
    /// The caller (typically the agentic loop in `Worker`) is responsible for
    /// executing tools and feeding results back.
    ///
    /// **Provider-specific behaviour for `system` roles:**
    /// - `OllamaClient` passes all messages (including `role: "system"`) directly in the
    ///   messages array, as Ollama's `/api/chat` endpoint accepts them inline.
    /// - `ClaudeClient` extracts the first `role: "system"` message and promotes it to the
    ///   top-level `system` field required by the Anthropic Messages API; any remaining system
    ///   messages are dropped. Build prompts with at most one system message at index 0.
    async fn chat(&self, req: ChatRequest) -> Result<(ChatResponse, CompletionStats), LlmError>;

    /// Generate embeddings for the given texts.
    ///
    /// **Not supported by all providers.** `ClaudeClient` always returns
    /// `Err(LlmError::ModelNotFound)` — embeddings must go through Ollama
    /// (`nomic-embed-text` or similar).
    async fn embed(&self, model: &str, input: &[String]) -> Result<Vec<Vec<f32>>, LlmError>;

    /// List available models.
    async fn list_models(&self) -> Result<Vec<String>, LlmError>;

    /// Health check — is the provider reachable?
    async fn ping(&self) -> Result<(), LlmError>;

    /// Human-readable provider name.
    fn provider_name(&self) -> &str;
}
