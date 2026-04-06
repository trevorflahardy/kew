//! Chain execution: run tasks sequentially, passing each result as context to the next.

use std::sync::Arc;
use tracing::{info, instrument};

use crate::db::models::{NewTask, Provider};
use crate::db::{self, Database};
use crate::llm::LlmClient;

use super::worker::{WorkResult, Worker};

/// A step in a chain.
#[derive(Debug, Clone)]
pub struct ChainStep {
    pub prompt: String,
    pub model: String,
    pub provider: Provider,
    pub system_prompt: Option<String>,
}

/// Execute a chain of steps sequentially.
///
/// Each step's output is stored as a context entry under the key
/// `"{chain_id}-step-{N}"` (zero-indexed). The next step automatically loads
/// the previous step's context key, injecting the result into its prompt as
/// `[Shared context: {chain_id}-step-{N}]: {output}`.
///
/// **Early termination:** if any step fails, execution stops immediately and
/// results collected so far are returned. Check `WorkResult::result` for
/// `Err` to detect which step failed.
///
/// `chain_id` should be a unique identifier for this chain run (e.g. a ULID).
/// It is used only as a namespace prefix for context keys — it is not persisted
/// anywhere else.
#[instrument(skip(db, ollama, claude, steps), fields(chain_len = steps.len()))]
pub async fn execute_chain(
    db: &Database,
    ollama: Arc<dyn LlmClient>,
    claude: Option<Arc<dyn LlmClient>>,
    steps: Vec<ChainStep>,
    chain_id: &str,
) -> Vec<WorkResult> {
    let worker = Worker::new("chain-worker".into(), db.clone(), ollama, claude);
    let mut results = Vec::with_capacity(steps.len());

    for (i, step) in steps.iter().enumerate() {
        info!(step = i, prompt = %step.prompt, "executing chain step");

        // Build context keys: include previous step's shared context
        let mut context_keys = Vec::new();
        if i > 0 {
            context_keys.push(format!("{chain_id}-step-{}", i - 1));
        }

        // Create and claim the task
        let task = {
            let conn = db.conn();
            let new = NewTask {
                model: step.model.clone(),
                provider: step.provider.clone(),
                prompt: step.prompt.clone(),
                system_prompt: step.system_prompt.clone(),
                context_keys,
                share_as: Some(format!("{chain_id}-step-{i}")),
                files_locked: vec![],
                parent_id: None,
                chain_id: Some(chain_id.to_string()),
                chain_index: Some(i as i32),
            };
            db::tasks::create_task(&conn, &new).unwrap();
            db::tasks::claim_next_pending(&conn, "chain-worker")
                .unwrap()
                .expect("just-created task should be claimable")
        };

        let result = worker.execute(&task).await;

        // If a step fails, stop the chain
        if result.result.is_err() {
            results.push(result);
            break;
        }

        results.push(result);
    }

    info!(completed = results.len(), "chain finished");
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, ChatRequest, ChatResponse, CompletionStats, LlmError};

    /// Echo client that returns all user messages joined — lets us verify
    /// context injection between chain steps.
    struct EchoClient;

    #[async_trait::async_trait]
    impl LlmClient for EchoClient {
        async fn chat(
            &self,
            req: ChatRequest,
        ) -> Result<(ChatResponse, CompletionStats), LlmError> {
            let user_msgs: Vec<String> = req
                .messages
                .iter()
                .filter(|m| m.role == "user")
                .map(|m| m.content.clone())
                .collect();
            Ok((
                ChatResponse {
                    message: ChatMessage {
                        role: "assistant".into(),
                        content: user_msgs.join(" | "),
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

    #[tokio::test]
    async fn test_chain_passes_context() {
        let db = Database::open_in_memory().unwrap();
        let client: Arc<dyn LlmClient> = Arc::new(EchoClient);

        let steps = vec![
            ChainStep {
                prompt: "step-0-prompt".into(),
                model: "echo".into(),
                provider: Provider::Ollama,
                system_prompt: None,
            },
            ChainStep {
                prompt: "step-1-prompt".into(),
                model: "echo".into(),
                provider: Provider::Ollama,
                system_prompt: None,
            },
        ];

        let results = execute_chain(&db, client, None, steps, "test-chain").await;
        assert_eq!(results.len(), 2);

        // Step 0: just its own prompt
        let r0 = results[0].result.as_ref().unwrap();
        assert!(
            r0.contains("step-0-prompt"),
            "step 0 should have its prompt: {r0}"
        );

        // Step 1: should have step 0's output as context + its own prompt
        let r1 = results[1].result.as_ref().unwrap();
        assert!(
            r1.contains("step-1-prompt"),
            "step 1 should have its prompt: {r1}"
        );
        assert!(
            r1.contains("step-0-prompt"),
            "step 1 should see step 0's result as context: {r1}"
        );
    }

    #[tokio::test]
    async fn test_chain_stops_on_failure() {
        struct FailOnSecondCall {
            count: std::sync::atomic::AtomicUsize,
        }

        #[async_trait::async_trait]
        impl LlmClient for FailOnSecondCall {
            async fn chat(
                &self,
                _req: ChatRequest,
            ) -> Result<(ChatResponse, CompletionStats), LlmError> {
                let n = self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n >= 1 {
                    return Err(LlmError::ProviderError {
                        status: 500,
                        body: "forced failure".into(),
                    });
                }
                Ok((
                    ChatResponse {
                        message: ChatMessage {
                            role: "assistant".into(),
                            content: "ok".into(),
                        },
                        model: "mock".into(),
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
                "mock"
            }
        }

        let db = Database::open_in_memory().unwrap();
        let client: Arc<dyn LlmClient> = Arc::new(FailOnSecondCall {
            count: std::sync::atomic::AtomicUsize::new(0),
        });

        let steps = vec![
            ChainStep {
                prompt: "step 0".into(),
                model: "mock".into(),
                provider: Provider::Ollama,
                system_prompt: None,
            },
            ChainStep {
                prompt: "step 1".into(),
                model: "mock".into(),
                provider: Provider::Ollama,
                system_prompt: None,
            },
            ChainStep {
                prompt: "step 2".into(),
                model: "mock".into(),
                provider: Provider::Ollama,
                system_prompt: None,
            },
        ];

        let results = execute_chain(&db, client, None, steps, "fail-chain").await;
        // Should have 2 results: step 0 OK, step 1 failed, step 2 never ran
        assert_eq!(results.len(), 2);
        assert!(results[0].result.is_ok());
        assert!(results[1].result.is_err());
    }
}
