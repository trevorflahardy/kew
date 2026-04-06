//! Worker pool: manages N concurrent workers as tokio tasks.

use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::db::models::Task;
use crate::db::Database;
use crate::llm::LlmClient;

use super::worker::{WorkResult, Worker};

/// A pool of workers that process tasks concurrently.
pub struct Pool {
    db: Database,
    ollama: Arc<dyn LlmClient>,
    claude: Option<Arc<dyn LlmClient>>,
    size: usize,
    task_tx: mpsc::Sender<Task>,
    task_rx: Option<mpsc::Receiver<Task>>,
    result_tx: mpsc::Sender<WorkResult>,
    result_rx: Option<mpsc::Receiver<WorkResult>>,
}

impl Pool {
    pub fn new(
        db: Database,
        ollama: Arc<dyn LlmClient>,
        claude: Option<Arc<dyn LlmClient>>,
        size: usize,
    ) -> Self {
        let (task_tx, task_rx) = mpsc::channel(size * 2);
        let (result_tx, result_rx) = mpsc::channel(size * 2);

        Self {
            db,
            ollama,
            claude,
            size,
            task_tx,
            task_rx: Some(task_rx),
            result_tx,
            result_rx: Some(result_rx),
        }
    }

    /// Start the worker pool. Spawns N worker tokio tasks that pull
    /// from the task channel and push results to the result channel.
    ///
    /// Returns a handle to the result receiver.
    pub fn start(&mut self) -> mpsc::Receiver<WorkResult> {
        let task_rx = self.task_rx.take().expect("pool already started");
        let task_rx = Arc::new(tokio::sync::Mutex::new(task_rx));
        let result_rx = self.result_rx.take().expect("pool already started");

        for i in 0..self.size {
            let worker_id = format!("worker-{i}");
            let db = self.db.clone();
            let ollama = self.ollama.clone();
            let claude = self.claude.clone();
            let task_rx = task_rx.clone();
            let result_tx = self.result_tx.clone();

            tokio::spawn(async move {
                let worker = Worker::new(worker_id.clone(), db, ollama, claude);
                debug!(worker = %worker_id, "worker started");

                loop {
                    let task = {
                        let mut rx = task_rx.lock().await;
                        rx.recv().await
                    };

                    match task {
                        Some(task) => {
                            let result = worker.execute(&task).await;
                            if result_tx.send(result).await.is_err() {
                                break; // Result receiver dropped
                            }
                        }
                        None => break, // Task channel closed — shutdown
                    }
                }

                debug!(worker = %worker_id, "worker stopped");
            });
        }

        info!(workers = self.size, "pool started");
        result_rx
    }

    /// Submit a task to the pool.
    pub async fn submit(&self, task: Task) -> Result<(), mpsc::error::SendError<Task>> {
        self.task_tx.send(task).await
    }

    /// Submit a task and wait for its result. Convenience method for single-task execution.
    pub async fn submit_and_wait(&mut self, task: Task) -> WorkResult {
        let mut result_rx = self.start();
        self.task_tx.send(task).await.expect("pool channel closed");
        // Drop the sender so workers know no more tasks are coming
        drop(self.task_tx.clone()); // Clone first since we need it for the struct
        result_rx.recv().await.expect("no result received")
    }

    /// Submit multiple tasks and wait for all results.
    pub async fn submit_all_and_wait(&mut self, tasks: Vec<Task>) -> Vec<WorkResult> {
        let count = tasks.len();
        let mut result_rx = self.start();

        for task in tasks {
            self.task_tx.send(task).await.expect("pool channel closed");
        }

        // Close the task channel to signal workers
        // (They'll finish current work then stop)
        let sender = self.task_tx.clone();
        drop(sender);

        let mut results = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some(result) = result_rx.recv().await {
                results.push(result);
            }
        }

        info!(count = results.len(), "all tasks completed");
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{NewTask, Provider};
    use crate::db::tasks;
    use crate::llm::{
        ChatMessage, ChatRequest, ChatResponse, CompletionStats, LlmClient, LlmError,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct CountingClient {
        call_count: AtomicUsize,
        latency: Duration,
    }

    #[async_trait::async_trait]
    impl LlmClient for CountingClient {
        async fn chat(
            &self,
            _req: ChatRequest,
        ) -> Result<(ChatResponse, CompletionStats), LlmError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.latency).await;
            Ok((
                ChatResponse {
                    message: ChatMessage {
                        role: "assistant".into(),
                        content: "done".into(),
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
            "counting"
        }
    }

    fn create_test_tasks(db: &Database, count: usize) -> Vec<Task> {
        let conn = db.conn();
        let mut tasks_out = Vec::new();
        for i in 0..count {
            let new = NewTask {
                model: "mock".into(),
                provider: Provider::Ollama,
                prompt: format!("Task {i}"),
                system_prompt: None,
                context_keys: vec![],
                share_as: None,
                files_locked: vec![],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            let _task = tasks::create_task(&conn, &new).unwrap();
            let claimed = tasks::claim_next_pending(&conn, &format!("w-{i}")).unwrap().unwrap();
            tasks_out.push(claimed);
        }
        tasks_out
    }

    #[tokio::test]
    async fn test_pool_executes_tasks() {
        let db = Database::open_in_memory().unwrap();
        let client: Arc<dyn LlmClient> = Arc::new(CountingClient {
            call_count: AtomicUsize::new(0),
            latency: Duration::from_millis(1),
        });

        let test_tasks = create_test_tasks(&db, 3);

        let mut pool = Pool::new(db, client.clone(), None, 2);
        let results = pool.submit_all_and_wait(test_tasks).await;

        assert_eq!(results.len(), 3);
        for r in &results {
            assert!(r.result.is_ok());
        }
    }
}
