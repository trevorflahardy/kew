//! Single task execution — THE critical function.
//!
//! A Worker takes a Task, loads context, builds the message array,
//! calls the LLM, stores the result, and optionally shares it as context.

use std::sync::Arc;
use tracing::{debug, error, info, instrument};

use crate::db::models::{Provider, Task};
use crate::db::{self, Database};
use crate::llm::{ChatMessage, ChatRequest, CompletionStats, LlmClient, LlmError};

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
            Provider::Claude => self
                .claude
                .as_ref()
                .map(|c| c.as_ref())
                .ok_or_else(|| LlmError::ModelNotFound("Claude API client not configured".into())),
        }
    }

    /// Execute a task end-to-end. This is THE critical function.
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
            messages.push(ChatMessage {
                role: "system".into(),
                content: sys.clone(),
            });
        }

        // Inject context as user messages
        for entry in &context_entries {
            messages.push(ChatMessage {
                role: "user".into(),
                content: format!("[Shared context: {}]\n{}", entry.key, entry.content),
            });
        }

        // The actual user prompt
        messages.push(ChatMessage {
            role: "user".into(),
            content: task.prompt.clone(),
        });

        // 4. Acquire file locks
        if !task.files_locked.is_empty() {
            let conn = self.db.conn();
            for path in &task.files_locked {
                if !db::locks::acquire_lock(&conn, path, &task.id, 600).unwrap_or(false) {
                    let err = format!("could not acquire lock on {path}");
                    self.fail_task(&task.id, &err);
                    return WorkResult {
                        task_id: task.id.clone(),
                        result: Err(err),
                        stats: CompletionStats::default(),
                    };
                }
            }
        }

        // 5. CALL THE LLM
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

        let chat_req = ChatRequest {
            model: task.model.clone(),
            messages,
            stream: false,
            temperature: Some(0.3),
            max_tokens: Some(4096),
        };

        let llm_result = client.chat(chat_req).await;

        // 6. Handle result
        let work_result = match llm_result {
            Ok((response, stats)) => {
                let result_text = response.message.content.clone();

                // Store result in DB
                {
                    let conn = self.db.conn();
                    if let Err(e) = db::tasks::mark_done(
                        &conn,
                        &task.id,
                        &result_text,
                        stats.prompt_tokens,
                        stats.completion_tokens,
                        stats.duration_ms,
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

                // Auto-embed result for future vector search (best-effort)
                self.try_embed_result(&task.id, &task.prompt, &result_text).await;

                info!(duration_ms = stats.duration_ms, "task completed");
                WorkResult {
                    task_id: task.id.clone(),
                    result: Ok(result_text),
                    stats,
                }
            }
            Err(e) => {
                let err = e.to_string();
                self.fail_task(&task.id, &err);
                error!("LLM call failed: {err}");
                WorkResult {
                    task_id: task.id.clone(),
                    result: Err(err),
                    stats: CompletionStats::default(),
                }
            }
        };

        // 7. Release file locks
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
                    debug!("failed to store embedding: {e}");
                }
            }
            Ok(_) => {
                debug!("embedding model returned empty vector, skipping");
            }
            Err(e) => {
                debug!("embedding failed (non-fatal): {e}");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::TaskStatus;
    use crate::llm::{ChatRequest, ChatResponse, CompletionStats, LlmError};
    use std::time::Duration;

    /// Mock LLM client for testing.
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
                    message: ChatMessage {
                        role: "assistant".into(),
                        content: self.response.clone(),
                    },
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
        let ctx = db::context::get_context(&conn, "auth-analysis").unwrap().unwrap();
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
                // Return all messages concatenated so we can verify context was injected
                let all_content: Vec<String> =
                    req.messages.iter().map(|m| m.content.clone()).collect();
                Ok((
                    ChatResponse {
                        message: ChatMessage {
                            role: "assistant".into(),
                            content: all_content.join("\n---\n"),
                        },
                        model: "echo".into(),
                        done: true,
                        total_duration_ns: None,
                        prompt_eval_count: None,
                        eval_count: None,
                    },
                    CompletionStats::default(),
                ))
            }
            async fn embed(
                &self,
                _model: &str,
                _input: &[String],
            ) -> Result<Vec<Vec<f32>>, LlmError> {
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

        // Verify: system prompt, context, and user prompt all present
        assert!(output.contains("You are a tester."));
        assert!(output.contains("The auth uses JWT."));
        assert!(output.contains("Write tests"));
    }
}
