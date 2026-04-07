//! Claude API client — Anthropic Messages API.
//!
//! Implements the LlmClient trait for Claude models via the Anthropic API.
//!
//! ## Tool calling
//!
//! Claude uses a different wire format than Ollama for tool calling:
//! - Tools are sent with `input_schema` (not `parameters`)
//! - Tool calls come back as `tool_use` content blocks
//! - Tool results are `tool_result` content blocks in a user message
//!
//! This client translates between the internal `ToolDefinition`/`ToolCall` types
//! and Claude's native format.

use async_trait::async_trait;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::time::Instant;

use super::{
    ChatMessage, ChatRequest, ChatResponse, CompletionStats, LlmClient, LlmError, ToolCall,
    ToolCallFunction, ToolDefinition,
};

pub struct ClaudeClient {
    client: Client,
    api_key: SecretString,
    base_url: String,
}

impl ClaudeClient {
    pub fn new(api_key: &str) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .expect("failed to build HTTP client"),
            api_key: SecretString::from(api_key.to_string()),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ClaudeTool>>,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    content: ApiContent,
}

/// Claude messages use either a plain string or an array of content blocks.
#[derive(Serialize)]
#[serde(untagged)]
enum ApiContent {
    Text(String),
    Blocks(Vec<ContentBlockInput>),
}

/// Content blocks sent TO Claude (tool results, mixed text + tool_result).
#[derive(Serialize)]
#[serde(tag = "type")]
enum ContentBlockInput {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

/// Claude tool definition — uses `input_schema` instead of `parameters`.
#[derive(Serialize)]
struct ClaudeTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

impl From<&ToolDefinition> for ClaudeTool {
    fn from(td: &ToolDefinition) -> Self {
        Self {
            name: td.function.name.clone(),
            description: td.function.description.clone(),
            input_schema: td.function.parameters.clone(),
        }
    }
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
    model: String,
    usage: Usage,
    #[serde(default)]
    stop_reason: Option<String>,
}

/// Content blocks received FROM Claude.
#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        /// Tool use ID — used to correlate results. Kept for deserialization but
        /// not referenced directly (we match by tool name instead).
        #[allow(dead_code)]
        id: String,
        name: String,
        input: serde_json::Value,
    },
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

// --- Message building helpers ---

/// Convert internal ChatMessages into Claude API messages.
///
/// Handles the translation of:
/// - `role: "system"` → extracted to top-level system param (returned separately)
/// - `role: "assistant"` with `tool_calls` → assistant message with `tool_use` blocks
/// - `role: "tool"` → `tool_result` blocks in a user message
///
/// Tool result messages are grouped into a single user message with multiple
/// `tool_result` blocks, matching Claude's expected format.
fn build_claude_messages(messages: &[ChatMessage]) -> (Option<String>, Vec<ApiMessage>) {
    let mut system_prompt = None;
    let mut api_messages: Vec<ApiMessage> = Vec::new();
    // Buffer for consecutive tool results that need to be grouped into one user message
    let mut tool_result_buffer: Vec<ContentBlockInput> = Vec::new();

    for msg in messages {
        if msg.role == "system" {
            system_prompt = Some(msg.content.clone());
            continue;
        }

        // Flush any buffered tool results before a non-tool message
        if msg.role != "tool" && !tool_result_buffer.is_empty() {
            api_messages.push(ApiMessage {
                role: "user".into(),
                content: ApiContent::Blocks(std::mem::take(&mut tool_result_buffer)),
            });
        }

        if msg.role == "assistant" && msg.has_tool_calls() {
            // Assistant message with tool calls → tool_use content blocks
            let mut blocks = Vec::new();
            if !msg.content.is_empty() {
                blocks.push(ContentBlockInput::Text {
                    text: msg.content.clone(),
                });
            }
            if let Some(ref tool_calls) = msg.tool_calls {
                for tc in tool_calls {
                    // Generate a stable ID from the function name + index
                    let id = format!("toolu_{}", tc.function.name);
                    blocks.push(ContentBlockInput::ToolUse {
                        id,
                        name: tc.function.name.clone(),
                        input: tc.function.arguments.clone(),
                    });
                }
            }
            api_messages.push(ApiMessage {
                role: "assistant".into(),
                content: ApiContent::Blocks(blocks),
            });
        } else if msg.role == "tool" {
            // Tool result → buffer as tool_result block
            let tool_use_id = format!(
                "toolu_{}",
                msg.tool_name.as_deref().unwrap_or("unknown")
            );
            tool_result_buffer.push(ContentBlockInput::ToolResult {
                tool_use_id,
                content: msg.content.clone(),
            });
        } else {
            // Plain user/assistant text message
            api_messages.push(ApiMessage {
                role: msg.role.clone(),
                content: ApiContent::Text(msg.content.clone()),
            });
        }
    }

    // Flush remaining tool results
    if !tool_result_buffer.is_empty() {
        api_messages.push(ApiMessage {
            role: "user".into(),
            content: ApiContent::Blocks(tool_result_buffer),
        });
    }

    (system_prompt, api_messages)
}

#[async_trait]
impl LlmClient for ClaudeClient {
    async fn chat(&self, req: ChatRequest) -> Result<(ChatResponse, CompletionStats), LlmError> {
        let start = Instant::now();

        let (system_prompt, api_messages) = build_claude_messages(&req.messages);

        // Convert tool definitions to Claude format
        let tools = req.tools.as_ref().map(|defs| {
            defs.iter().map(ClaudeTool::from).collect::<Vec<_>>()
        }).filter(|t| !t.is_empty());

        let body = MessagesRequest {
            model: req.model,
            max_tokens: req.max_tokens.unwrap_or(4096),
            messages: api_messages,
            system: system_prompt,
            temperature: req.temperature,
            tools,
        };

        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", self.api_key.expose_secret())
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

        // Extract text and tool calls from content blocks
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in &msg_response.content {
            match block {
                ContentBlock::Text { text } => text_parts.push(text.as_str()),
                ContentBlock::ToolUse {
                    name, input, ..
                } => {
                    tool_calls.push(ToolCall {
                        call_type: "function".into(),
                        function: ToolCallFunction {
                            name: name.clone(),
                            arguments: input.clone(),
                        },
                    });
                }
            }
        }

        let text = text_parts.join("");
        let has_tool_calls = !tool_calls.is_empty();

        let chat_response = ChatResponse {
            message: ChatMessage {
                role: "assistant".into(),
                content: text,
                tool_calls: if has_tool_calls {
                    Some(tool_calls)
                } else {
                    None
                },
                tool_name: None,
            },
            model: msg_response.model,
            done: msg_response.stop_reason.as_deref() != Some("tool_use"),
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
        Err(LlmError::ModelNotFound(
            "Claude API does not support embeddings. Use Ollama with nomic-embed-text.".into(),
        ))
    }

    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        Ok(vec![
            "claude-sonnet-4-20250514".into(),
            "claude-haiku-4-5-20251001".into(),
            "claude-opus-4-20250514".into(),
        ])
    }

    async fn ping(&self) -> Result<(), LlmError> {
        let response = self
            .client
            .get(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", self.api_key.expose_secret())
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .map_err(|e| LlmError::Unreachable {
                url: self.base_url.clone(),
                source: e,
            })?;

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
                content: ApiContent::Text("Hello".into()),
            }],
            system: Some("Be helpful".into()),
            temperature: Some(0.3),
            tools: None,
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
            tools: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("system").is_none());
        assert!(json.get("temperature").is_none());
        assert!(json.get("tools").is_none());
    }

    #[test]
    fn test_content_block_deserialization() {
        let json = r#"[
            {"type": "text", "text": "Hello!"},
            {"type": "tool_use", "id": "toolu_abc", "name": "read_file", "input": {"path": "src/main.rs"}}
        ]"#;
        let blocks: Vec<ContentBlock> = serde_json::from_str(json).unwrap();
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello!"),
            _ => panic!("expected text block"),
        }
        match &blocks[1] {
            ContentBlock::ToolUse { name, input, .. } => {
                assert_eq!(name, "read_file");
                assert_eq!(input["path"], "src/main.rs");
            }
            _ => panic!("expected tool_use block"),
        }
    }

    #[test]
    fn test_build_claude_messages_extracts_system() {
        let messages = vec![
            ChatMessage::text("system", "Be helpful"),
            ChatMessage::text("user", "Hello"),
        ];
        let (system, api_msgs) = build_claude_messages(&messages);
        assert_eq!(system, Some("Be helpful".into()));
        assert_eq!(api_msgs.len(), 1);
        assert_eq!(api_msgs[0].role, "user");
    }

    #[test]
    fn test_build_claude_messages_tool_round_trip() {
        // Simulate: user asks → assistant calls tool → tool result → assistant answers
        let messages = vec![
            ChatMessage::text("user", "Read main.rs"),
            ChatMessage {
                role: "assistant".into(),
                content: String::new(),
                tool_calls: Some(vec![ToolCall {
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: "read_file".into(),
                        arguments: serde_json::json!({"path": "src/main.rs"}),
                    },
                }]),
                tool_name: None,
            },
            ChatMessage::tool_result("read_file", "fn main() {}"),
        ];

        let (_, api_msgs) = build_claude_messages(&messages);
        assert_eq!(api_msgs.len(), 3); // user, assistant(tool_use), user(tool_result)
        assert_eq!(api_msgs[0].role, "user");
        assert_eq!(api_msgs[1].role, "assistant");
        assert_eq!(api_msgs[2].role, "user"); // tool results become user messages
    }

    #[test]
    fn test_claude_tool_definition_conversion() {
        let td = ToolDefinition {
            tool_type: "function".into(),
            function: super::super::ToolFunction {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                }),
            },
        };
        let claude_tool = ClaudeTool::from(&td);
        assert_eq!(claude_tool.name, "read_file");
        assert_eq!(claude_tool.input_schema["type"], "object");
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
