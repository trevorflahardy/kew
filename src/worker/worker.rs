//! Single task execution — THE critical function.
//!
//! A Worker takes a Task, loads context, builds the message array,
//! calls the LLM, stores the result, and optionally shares it as context.
//!
//! ## Agentic tool loop
//!
//! When tool definitions are available, the worker runs a multi-turn loop:
//!
//! 1. Build initial messages (system prompt, context, files, user prompt)
//! 2. Send to LLM with tool definitions
//! 3. If the response contains `tool_calls`:
//!    a. Execute each tool call via the `ToolSandbox`
//!    b. Append the assistant message and tool results to the conversation
//!    c. Send the updated conversation back to the LLM (goto 2)
//! 4. If the response is a plain text reply, we're done — store the result.
//!
//! The loop is capped at `MAX_TOOL_ITERATIONS` (25) to prevent runaway agents.
//! Stats are accumulated across all iterations.

use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

use crate::db::models::{Provider, Task};
use crate::db::{self, Database};
use crate::llm::{ChatMessage, ChatRequest, CompletionStats, LlmClient, LlmError};
use crate::worker::tools::{ToolSandbox, MAX_TOOL_ITERATIONS};

/// Result of a single task execution.
#[derive(Debug, Clone)]
pub struct WorkResult {
    pub task_id: String,
    pub result: Result<String, String>,
    pub stats: CompletionStats,
}

/// A worker that executes a single task against an LLM.
pub struct Worker {
    pub id: String,
    db: Database,
    ollama: Arc<dyn LlmClient>,
    claude: Option<Arc<dyn LlmClient>>,
}

impl Worker {
    pub fn new(
        id: String,
        db: Database,
        ollama: Arc<dyn LlmClient>,
        claude: Option<Arc<dyn LlmClient>>,
    ) -> Self {
        Self {
            id,
            db,
            ollama,
            claude,
        }
    }

    /// Pick the right client for the task's provider.
    fn client_for(&self, provider: &Provider) -> Result<&dyn LlmClient, LlmError> {
        match provider {
            Provider::Ollama => Ok(self.ollama.as_ref()),
            Provider::Claude => {
                self.claude.as_ref().map(|c| c.as_ref()).ok_or_else(|| {
                    LlmError::ModelNotFound("Claude API client not configured".into())
                })
            }
        }
    }

    /// Execute a task end-to-end. This is THE critical function.
    ///
    /// **Side effects** (always happen, regardless of success or failure):
    /// - Marks the task `running` in the database at the start.
    /// - Marks the task `done` or `failed` in the database at the end.
    /// - Releases all file locks held by the task when execution finishes.
    /// - On success, stores the result as a context entry if `task.share_as` is set.
    /// - On success, best-effort embeds the result via Ollama for future vector search
    ///   (failures here are logged and ignored).
    ///
    /// **Agentic tool loop:** When running against a model that supports tool calling,
    /// the worker provides `read_file`, `list_dir`, `grep`, and `write_file` tools.
    /// The model can call these mid-generation to explore the codebase. The loop runs
    /// until the model produces a final text response or hits the iteration cap.
    ///
    /// **Hardcoded LLM parameters:** `temperature = 0.3`, `max_tokens = 4096`. These
    /// are not task-configurable — all tasks use the same values regardless of model.
    #[instrument(skip(self, task), fields(task_id = %task.id, model = %task.model))]
    pub async fn execute(&self, task: &Task) -> WorkResult {
        info!("executing task");

        // 1. Mark task as running
        {
            let conn = self.db.conn();
            if let Err(e) = db::tasks::mark_running(&conn, &task.id) {
                error!("failed to mark task running: {e}");
            }
        }

        // 2. Load explicit context
        let context_entries = {
            let conn = self.db.conn();
            db::context::get_context_many(&conn, &task.context_keys).unwrap_or_default()
        };

        // 3. Build messages array
        let mut messages = Vec::new();

        // System prompt first
        if let Some(ref sys) = task.system_prompt {
            messages.push(ChatMessage::text("system", sys.clone()));
        }

        // Inject context as user messages
        for entry in &context_entries {
            messages.push(ChatMessage::text(
                "user",
                format!("[Shared context: {}]\n{}", entry.key, entry.content),
            ));
        }

        // The actual user prompt
        messages.push(ChatMessage::text("user", task.prompt.clone()));

        // 4. Acquire file locks
        if !task.files_locked.is_empty() {
            let lock_err = {
                let conn = self.db.conn();
                let mut err = None;
                for path in &task.files_locked {
                    match db::locks::acquire_lock(&conn, path, &task.id, 600) {
                        Ok(true) => {} // acquired
                        Ok(false) => {
                            err = Some(format!("could not acquire lock on {path}"));
                            break;
                        }
                        Err(e) => {
                            err = Some(format!("lock DB error on {path}: {e}"));
                            break;
                        }
                    }
                }
                err
            }; // conn dropped here
            if let Some(err) = lock_err {
                self.fail_task(&task.id, &err);
                return WorkResult {
                    task_id: task.id.clone(),
                    result: Err(err),
                    stats: CompletionStats::default(),
                };
            }
        }

        // 5. Set up tool sandbox and definitions for the agentic loop
        let project_root = std::env::current_dir().unwrap_or_default();
        let tool_sandbox = ToolSandbox::new(project_root, task.id.clone(), self.db.clone());
        let tool_defs = ToolSandbox::definitions();

        // 6. AGENTIC TOOL LOOP — call LLM, execute tools, repeat
        debug!("calling LLM: {} via {:?}", task.model, task.provider);
        let client = match self.client_for(&task.provider) {
            Ok(c) => c,
            Err(e) => {
                let err = e.to_string();
                self.fail_task(&task.id, &err);
                self.release_locks(task);
                return WorkResult {
                    task_id: task.id.clone(),
                    result: Err(err),
                    stats: CompletionStats::default(),
                };
            }
        };

        let mut cumulative_stats = CompletionStats::default();
        let mut final_text: Option<String> = None;
        let mut loop_error: Option<String> = None;

        for iteration in 0..MAX_TOOL_ITERATIONS {
            debug!(iteration, "agentic loop iteration");

            let chat_req = ChatRequest {
                model: task.model.clone(),
                messages: messages.clone(),
                stream: false,
                temperature: Some(0.3),
                max_tokens: Some(4096),
                tools: Some(tool_defs.clone()),
            };

            match client.chat(chat_req).await {
                Ok((response, stats)) => {
                    accumulate_stats(&mut cumulative_stats, &stats);

                    if response.message.has_tool_calls() {
                        // Model wants to call tools — execute them and continue
                        let tool_calls = response.message.tool_calls.as_ref().unwrap();
                        debug!(count = tool_calls.len(), iteration, "executing tool calls");

                        // Append the assistant's tool-call message to the conversation
                        messages.push(response.message.clone());

                        // Execute each tool and append results
                        for tc in tool_calls {
                            let result = tool_sandbox.execute(tc);
                            debug!(
                                tool = %tc.function.name,
                                result_len = result.len(),
                                "tool executed"
                            );
                            messages.push(ChatMessage::tool_result(&tc.function.name, result));
                        }
                        // Continue loop — send updated conversation back to LLM
                    } else {
                        // Final text response — we're done
                        final_text = Some(response.message.content);
                        break;
                    }
                }
                Err(e) => {
                    loop_error = Some(e.to_string());
                    break;
                }
            }

            // Safety: if we've hit the last iteration, force the final response
            if iteration == MAX_TOOL_ITERATIONS - 1 {
                warn!("agentic loop hit iteration cap ({MAX_TOOL_ITERATIONS}), forcing final response");
                // Send one more request without tools to force a text response
                let final_req = ChatRequest {
                    model: task.model.clone(),
                    messages: messages.clone(),
                    stream: false,
                    temperature: Some(0.3),
                    max_tokens: Some(4096),
                    tools: None, // No tools — force text output
                };
                match client.chat(final_req).await {
                    Ok((response, stats)) => {
                        accumulate_stats(&mut cumulative_stats, &stats);
                        final_text = Some(response.message.content);
                    }
                    Err(e) => {
                        loop_error = Some(e.to_string());
                    }
                }
            }
        }

        // 7. Handle result
        let work_result = if let Some(result_text) = final_text {
            // Re-check task status — it may have been cancelled during the LLM await.
            // If it's no longer 'running', skip mark_done and context storage to avoid
            // polluting shared context with results from a cancelled task.
            let still_running = {
                let conn = self.db.conn();
                db::tasks::get_task(&conn, &task.id)
                    .ok()
                    .flatten()
                    .map(|t| t.status == crate::db::models::TaskStatus::Running)
                    .unwrap_or(false)
            };

            if still_running {
                // Store result in DB
                {
                    let conn = self.db.conn();
                    if let Err(e) = db::tasks::mark_done(
                        &conn,
                        &task.id,
                        &result_text,
                        cumulative_stats.prompt_tokens,
                        cumulative_stats.completion_tokens,
                        cumulative_stats.duration_ms,
                    ) {
                        error!("failed to mark task done: {e}");
                    }
                }

                // Share result as context if requested
                if let Some(ref share_key) = task.share_as {
                    let conn = self.db.conn();
                    if let Err(e) = db::context::put_context(
                        &conn,
                        share_key,
                        "default",
                        &result_text,
                        Some(&task.id),
                    ) {
                        error!("failed to share context as '{share_key}': {e}");
                    }
                }
            } else {
                debug!(task_id = %task.id, "task was cancelled during LLM call; skipping mark_done and context storage");
            }

            // Auto-embed result for future vector search (best-effort)
            self.try_embed_result(&task.id, &task.prompt, &result_text)
                .await;

            info!(duration_ms = cumulative_stats.duration_ms, "task completed");
            WorkResult {
                task_id: task.id.clone(),
                result: Ok(result_text),
                stats: cumulative_stats,
            }
        } else {
            let err = loop_error.unwrap_or_else(|| "agentic loop ended without a result".into());
            self.fail_task(&task.id, &err);
            error!("LLM call failed: {err}");
            WorkResult {
                task_id: task.id.clone(),
                result: Err(err),
                stats: cumulative_stats,
            }
        };

        // 8. Release file locks
        self.release_locks(task);

        work_result
    }

    /// Attempt to embed the task result for future vector search.
    /// Best-effort: embedding failures are logged but don't affect the task.
    async fn try_embed_result(&self, task_id: &str, prompt: &str, result: &str) {
        // Embed the concatenation of prompt + result for richer semantic search
        let text = format!("{prompt}\n\n{result}");
        let embed_result = self.ollama.embed("nomic-embed-text", &[text]).await;

        match embed_result {
            Ok(embeddings) if !embeddings.is_empty() && !embeddings[0].is_empty() => {
                let conn = self.db.conn();
                if let Err(e) = db::vectors::store_embedding(
                    &conn,
                    task_id,
                    "result",
                    Some(task_id),
                    &embeddings[0],
                    "nomic-embed-text",
                ) {
                    warn!("failed to store embedding: {e}");
                }
            }
            Ok(_) => {
                warn!("embedding model returned empty vector, skipping");
            }
            Err(e) => {
                warn!("embedding failed (non-fatal): {e}");
            }
        }
    }

    fn fail_task(&self, task_id: &str, error: &str) {
        let conn = self.db.conn();
        let _ = db::tasks::mark_failed(&conn, task_id, error);
    }

    fn release_locks(&self, task: &Task) {
        if !task.files_locked.is_empty() {
            let conn = self.db.conn();
            let _ = db::locks::release_all_locks(&conn, &task.id);
        }
    }
}

/// Accumulate stats across multiple LLM calls in the agentic loop.
fn accumulate_stats(cumulative: &mut CompletionStats, new: &CompletionStats) {
    cumulative.prompt_tokens = match (cumulative.prompt_tokens, new.prompt_tokens) {
        (Some(a), Some(b)) => Some(a + b),
        (Some(a), None) => Some(a),
        (None, b) => b,
    };
    cumulative.completion_tokens = match (cumulative.completion_tokens, new.completion_tokens) {
        (Some(a), Some(b)) => Some(a + b),
        (Some(a), None) => Some(a),
        (None, b) => b,
    };
    cumulative.duration_ms = match (cumulative.duration_ms, new.duration_ms) {
        (Some(a), Some(b)) => Some(a + b),
        (Some(a), None) => Some(a),
        (None, b) => b,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::TaskStatus;
    use crate::llm::{ChatRequest, ChatResponse, CompletionStats, LlmError};
    use std::time::Duration;

    /// Mock LLM client for testing — always returns a plain text response (no tool calls).
    struct MockLlmClient {
        response: String,
        latency: Duration,
    }

    #[async_trait::async_trait]
    impl LlmClient for MockLlmClient {
        async fn chat(
            &self,
            _req: ChatRequest,
        ) -> Result<(ChatResponse, CompletionStats), LlmError> {
            tokio::time::sleep(self.latency).await;
            Ok((
                ChatResponse {
                    message: ChatMessage::text("assistant", self.response.clone()),
                    model: "mock".into(),
                    done: true,
                    total_duration_ns: None,
                    prompt_eval_count: Some(10),
                    eval_count: Some(20),
                },
                CompletionStats {
                    prompt_tokens: Some(10),
                    completion_tokens: Some(20),
                    duration_ms: Some(100),
                },
            ))
        }

        async fn embed(&self, _model: &str, _input: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            Ok(vec![vec![0.0; 768]])
        }

        async fn list_models(&self) -> Result<Vec<String>, LlmError> {
            Ok(vec!["mock".into()])
        }

        async fn ping(&self) -> Result<(), LlmError> {
            Ok(())
        }

        fn provider_name(&self) -> &str {
            "mock"
        }
    }

    fn mock_client(response: &str) -> Arc<dyn LlmClient> {
        Arc::new(MockLlmClient {
            response: response.to_string(),
            latency: Duration::from_millis(1),
        })
    }

    #[tokio::test]
    async fn test_worker_executes_task() {
        let db = Database::open_in_memory().unwrap();
        let client = mock_client("print('hello world')");
        let worker = Worker::new("w1".into(), db.clone(), client, None);

        // Create a task
        let task = {
            let conn = db.conn();
            let new = crate::db::models::NewTask {
                model: "gemma4:26b".into(),
                provider: Provider::Ollama,
                prompt: "Write hello world in Python".into(),
                system_prompt: Some("You are a Python expert.".into()),
                context_keys: vec![],
                share_as: None,
                files_locked: vec![],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            db::tasks::create_task(&conn, &new).unwrap();
            db::tasks::claim_next_pending(&conn, "w1").unwrap().unwrap()
        };

        let result = worker.execute(&task).await;
        assert!(result.result.is_ok());
        assert_eq!(result.result.unwrap(), "print('hello world')");
        assert_eq!(result.stats.prompt_tokens, Some(10));

        // Verify DB was updated
        let conn = db.conn();
        let done_task = db::tasks::get_task(&conn, &task.id).unwrap().unwrap();
        assert_eq!(done_task.status, TaskStatus::Done);
        assert_eq!(done_task.result.as_deref(), Some("print('hello world')"));
    }

    #[tokio::test]
    async fn test_worker_shares_context() {
        let db = Database::open_in_memory().unwrap();
        let client = mock_client("JWT authentication is recommended");
        let worker = Worker::new("w1".into(), db.clone(), client, None);

        let task = {
            let conn = db.conn();
            let new = crate::db::models::NewTask {
                model: "gemma4:26b".into(),
                provider: Provider::Ollama,
                prompt: "Analyze auth module".into(),
                system_prompt: None,
                context_keys: vec![],
                share_as: Some("auth-analysis".into()),
                files_locked: vec![],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            db::tasks::create_task(&conn, &new).unwrap();
            db::tasks::claim_next_pending(&conn, "w1").unwrap().unwrap()
        };

        let result = worker.execute(&task).await;
        assert!(result.result.is_ok());

        // Verify context was shared
        let conn = db.conn();
        let ctx = db::context::get_context(&conn, "auth-analysis")
            .unwrap()
            .unwrap();
        assert_eq!(ctx.content, "JWT authentication is recommended");
        assert_eq!(ctx.created_by.as_deref(), Some(task.id.as_str()));
    }

    #[tokio::test]
    async fn test_worker_loads_context() {
        let db = Database::open_in_memory().unwrap();

        // Pre-populate context
        {
            let conn = db.conn();
            db::context::put_context(&conn, "prior-work", "default", "The auth uses JWT.", None)
                .unwrap();
        }

        // Use a client that echoes back what it receives
        struct EchoClient;
        #[async_trait::async_trait]
        impl LlmClient for EchoClient {
            async fn chat(
                &self,
                req: ChatRequest,
            ) -> Result<(ChatResponse, CompletionStats), LlmError> {
                let all_content: Vec<String> =
                    req.messages.iter().map(|m| m.content.clone()).collect();
                Ok((
                    ChatResponse {
                        message: ChatMessage::text("assistant", all_content.join("\n---\n")),
                        model: "echo".into(),
                        done: true,
                        total_duration_ns: None,
                        prompt_eval_count: None,
                        eval_count: None,
                    },
                    CompletionStats::default(),
                ))
            }
            async fn embed(&self, _: &str, _: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
                Ok(vec![])
            }
            async fn list_models(&self) -> Result<Vec<String>, LlmError> {
                Ok(vec![])
            }
            async fn ping(&self) -> Result<(), LlmError> {
                Ok(())
            }
            fn provider_name(&self) -> &str {
                "echo"
            }
        }

        let worker = Worker::new("w1".into(), db.clone(), Arc::new(EchoClient), None);

        let task = {
            let conn = db.conn();
            let new = crate::db::models::NewTask {
                model: "echo".into(),
                provider: Provider::Ollama,
                prompt: "Write tests".into(),
                system_prompt: Some("You are a tester.".into()),
                context_keys: vec!["prior-work".into()],
                share_as: None,
                files_locked: vec![],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            db::tasks::create_task(&conn, &new).unwrap();
            db::tasks::claim_next_pending(&conn, "w1").unwrap().unwrap()
        };

        let result = worker.execute(&task).await;
        let output = result.result.unwrap();

        assert!(output.contains("You are a tester."));
        assert!(output.contains("The auth uses JWT."));
        assert!(output.contains("Write tests"));
    }

    #[tokio::test]
    async fn test_worker_handles_llm_failure() {
        struct FailingClient;
        #[async_trait::async_trait]
        impl LlmClient for FailingClient {
            async fn chat(
                &self,
                _req: ChatRequest,
            ) -> Result<(ChatResponse, CompletionStats), LlmError> {
                Err(LlmError::ProviderError {
                    status: 500,
                    body: "internal error".into(),
                })
            }
            async fn embed(&self, _: &str, _: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
                Ok(vec![])
            }
            async fn list_models(&self) -> Result<Vec<String>, LlmError> {
                Ok(vec![])
            }
            async fn ping(&self) -> Result<(), LlmError> {
                Ok(())
            }
            fn provider_name(&self) -> &str {
                "failing"
            }
        }

        let db = Database::open_in_memory().unwrap();
        let worker = Worker::new("w1".into(), db.clone(), Arc::new(FailingClient), None);

        let task = {
            let conn = db.conn();
            let new = crate::db::models::NewTask {
                model: "failing".into(),
                provider: Provider::Ollama,
                prompt: "This will fail".into(),
                system_prompt: None,
                context_keys: vec![],
                share_as: None,
                files_locked: vec![],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            db::tasks::create_task(&conn, &new).unwrap();
            db::tasks::claim_next_pending(&conn, "w1").unwrap().unwrap()
        };

        let result = worker.execute(&task).await;
        assert!(result.result.is_err());
        assert!(result.result.unwrap_err().contains("internal error"));

        let conn = db.conn();
        let task = db::tasks::get_task(&conn, &result.task_id)
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert!(task.error.unwrap().contains("internal error"));
    }

    #[tokio::test]
    async fn test_worker_acquires_and_releases_file_locks() {
        let db = Database::open_in_memory().unwrap();
        let client = mock_client("done");
        let worker = Worker::new("w1".into(), db.clone(), client, None);

        let task = {
            let conn = db.conn();
            let new = crate::db::models::NewTask {
                model: "mock".into(),
                provider: Provider::Ollama,
                prompt: "Edit file".into(),
                system_prompt: None,
                context_keys: vec![],
                share_as: None,
                files_locked: vec!["src/main.rs".into()],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            db::tasks::create_task(&conn, &new).unwrap();
            db::tasks::claim_next_pending(&conn, "w1").unwrap().unwrap()
        };

        let result = worker.execute(&task).await;
        assert!(result.result.is_ok());

        let conn = db.conn();
        let locks = db::locks::list_locks(&conn).unwrap();
        assert!(locks.is_empty(), "locks should be released: {locks:?}");
    }

    #[tokio::test]
    async fn test_worker_fails_on_lock_conflict() {
        let db = Database::open_in_memory().unwrap();

        {
            let conn = db.conn();
            let new = crate::db::models::NewTask {
                model: "mock".into(),
                provider: Provider::Ollama,
                prompt: "blocker".into(),
                system_prompt: None,
                context_keys: vec![],
                share_as: None,
                files_locked: vec![],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            db::tasks::create_task(&conn, &new).unwrap();
            let blocker = db::tasks::claim_next_pending(&conn, "w0").unwrap().unwrap();
            db::locks::acquire_lock(&conn, "src/main.rs", &blocker.id, 600).unwrap();
        }

        let client = mock_client("done");
        let worker = Worker::new("w1".into(), db.clone(), client, None);

        let task = {
            let conn = db.conn();
            let new = crate::db::models::NewTask {
                model: "mock".into(),
                provider: Provider::Ollama,
                prompt: "Try edit locked file".into(),
                system_prompt: None,
                context_keys: vec![],
                share_as: None,
                files_locked: vec!["src/main.rs".into()],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            db::tasks::create_task(&conn, &new).unwrap();
            db::tasks::claim_next_pending(&conn, "w1").unwrap().unwrap()
        };

        let result = worker.execute(&task).await;
        assert!(result.result.is_err());
        assert!(result
            .result
            .unwrap_err()
            .contains("could not acquire lock"));
    }

    #[tokio::test]
    async fn test_worker_claude_provider_routing() {
        struct ClaudeMock;
        #[async_trait::async_trait]
        impl LlmClient for ClaudeMock {
            async fn chat(
                &self,
                _req: ChatRequest,
            ) -> Result<(ChatResponse, CompletionStats), LlmError> {
                Ok((
                    ChatResponse {
                        message: ChatMessage::text("assistant", "from claude"),
                        model: "claude-sonnet".into(),
                        done: true,
                        total_duration_ns: None,
                        prompt_eval_count: None,
                        eval_count: None,
                    },
                    CompletionStats::default(),
                ))
            }
            async fn embed(&self, _: &str, _: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
                Ok(vec![])
            }
            async fn list_models(&self) -> Result<Vec<String>, LlmError> {
                Ok(vec![])
            }
            async fn ping(&self) -> Result<(), LlmError> {
                Ok(())
            }
            fn provider_name(&self) -> &str {
                "claude"
            }
        }

        let db = Database::open_in_memory().unwrap();
        let ollama = mock_client("from ollama");
        let claude: Arc<dyn LlmClient> = Arc::new(ClaudeMock);
        let worker = Worker::new("w1".into(), db.clone(), ollama, Some(claude));

        let task = {
            let conn = db.conn();
            let new = crate::db::models::NewTask {
                model: "claude-sonnet-4-20250514".into(),
                provider: Provider::Claude,
                prompt: "Test claude routing".into(),
                system_prompt: None,
                context_keys: vec![],
                share_as: None,
                files_locked: vec![],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            db::tasks::create_task(&conn, &new).unwrap();
            db::tasks::claim_next_pending(&conn, "w1").unwrap().unwrap()
        };

        let result = worker.execute(&task).await;
        assert!(result.result.is_ok());
        assert_eq!(result.result.unwrap(), "from claude");
    }

    #[tokio::test]
    async fn test_worker_claude_not_configured() {
        let db = Database::open_in_memory().unwrap();
        let ollama = mock_client("from ollama");
        let worker = Worker::new("w1".into(), db.clone(), ollama, None);

        let task = {
            let conn = db.conn();
            let new = crate::db::models::NewTask {
                model: "claude-sonnet-4-20250514".into(),
                provider: Provider::Claude,
                prompt: "Test missing claude".into(),
                system_prompt: None,
                context_keys: vec![],
                share_as: None,
                files_locked: vec![],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            db::tasks::create_task(&conn, &new).unwrap();
            db::tasks::claim_next_pending(&conn, "w1").unwrap().unwrap()
        };

        let result = worker.execute(&task).await;
        assert!(result.result.is_err());
        assert!(result.result.unwrap_err().contains("not configured"));
    }

    // --- Agentic tool loop tests ---

    /// Mock LLM that returns tool calls for N rounds, then a final text answer.
    struct ToolCallingMock {
        rounds: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmClient for ToolCallingMock {
        async fn chat(
            &self,
            req: ChatRequest,
        ) -> Result<(ChatResponse, CompletionStats), LlmError> {
            let current = self
                .rounds
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            if current > 0 && req.tools.is_some() {
                // Return a tool call
                Ok((
                    ChatResponse {
                        message: ChatMessage {
                            role: "assistant".into(),
                            content: String::new(),
                            tool_calls: Some(vec![crate::llm::ToolCall {
                                call_type: "function".into(),
                                function: crate::llm::ToolCallFunction {
                                    name: "list_dir".into(),
                                    arguments: serde_json::json!({}),
                                },
                            }]),
                            tool_name: None,
                        },
                        model: "mock".into(),
                        done: false,
                        total_duration_ns: None,
                        prompt_eval_count: Some(5),
                        eval_count: Some(5),
                    },
                    CompletionStats {
                        prompt_tokens: Some(5),
                        completion_tokens: Some(5),
                        duration_ms: Some(50),
                    },
                ))
            } else {
                // Final text answer
                Ok((
                    ChatResponse {
                        message: ChatMessage::text("assistant", "Final answer after tool use"),
                        model: "mock".into(),
                        done: true,
                        total_duration_ns: None,
                        prompt_eval_count: Some(10),
                        eval_count: Some(15),
                    },
                    CompletionStats {
                        prompt_tokens: Some(10),
                        completion_tokens: Some(15),
                        duration_ms: Some(100),
                    },
                ))
            }
        }
        async fn embed(&self, _: &str, _: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            Ok(vec![vec![0.0; 768]])
        }
        async fn list_models(&self) -> Result<Vec<String>, LlmError> {
            Ok(vec!["mock".into()])
        }
        async fn ping(&self) -> Result<(), LlmError> {
            Ok(())
        }
        fn provider_name(&self) -> &str {
            "mock-tool-calling"
        }
    }

    #[tokio::test]
    async fn test_worker_agentic_tool_loop() {
        let db = Database::open_in_memory().unwrap();
        let client: Arc<dyn LlmClient> = Arc::new(ToolCallingMock {
            rounds: std::sync::atomic::AtomicUsize::new(2), // 2 tool rounds, then final
        });
        let worker = Worker::new("w1".into(), db.clone(), client, None);

        let task = {
            let conn = db.conn();
            let new = crate::db::models::NewTask {
                model: "mock".into(),
                provider: Provider::Ollama,
                prompt: "Explore the codebase".into(),
                system_prompt: None,
                context_keys: vec![],
                share_as: None,
                files_locked: vec![],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            db::tasks::create_task(&conn, &new).unwrap();
            db::tasks::claim_next_pending(&conn, "w1").unwrap().unwrap()
        };

        let result = worker.execute(&task).await;
        assert!(result.result.is_ok());
        assert_eq!(result.result.unwrap(), "Final answer after tool use");

        // Stats should be accumulated from all iterations (2 tool + 1 final = 3 calls)
        assert_eq!(result.stats.prompt_tokens, Some(20)); // 5+5+10
        assert_eq!(result.stats.completion_tokens, Some(25)); // 5+5+15
        assert_eq!(result.stats.duration_ms, Some(200)); // 50+50+100
    }

    #[tokio::test]
    async fn test_accumulate_stats() {
        let mut cum = CompletionStats::default();
        let s1 = CompletionStats {
            prompt_tokens: Some(10),
            completion_tokens: Some(20),
            duration_ms: Some(100),
        };
        let s2 = CompletionStats {
            prompt_tokens: Some(5),
            completion_tokens: None,
            duration_ms: Some(50),
        };
        accumulate_stats(&mut cum, &s1);
        accumulate_stats(&mut cum, &s2);
        assert_eq!(cum.prompt_tokens, Some(15));
        assert_eq!(cum.completion_tokens, Some(20));
        assert_eq!(cum.duration_ms, Some(150));
    }
}
