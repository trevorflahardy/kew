//! Worker pool: manages N concurrent workers as tokio tasks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

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
    started: AtomicBool,
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
            started: AtomicBool::new(false),
        }
    }

    /// Start the worker pool. Spawns N worker tokio tasks that pull from the task
    /// channel and push results to the result channel.
    ///
    /// Returns a handle to the result receiver. Callers must drive this receiver
    /// to completion or workers will stall when the result buffer fills.
    ///
    /// Returns an error if called more than once.
    pub fn start(&mut self) -> anyhow::Result<mpsc::Receiver<WorkResult>> {
        if self
            .started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            anyhow::bail!("pool already started");
        }
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
        Ok(result_rx)
    }

    /// Submit a task to the pool.
    pub async fn submit(&self, task: Task) -> Result<(), mpsc::error::SendError<Task>> {
        self.task_tx.send(task).await
    }

    /// Submit a single task, wait for its result, then shut down the pool.
    ///
    /// Internally calls `start()` and then drops a clone of the task sender to
    /// signal shutdown. This means the pool cannot be reused after this call —
    /// workers will exit after the task completes.
    pub async fn submit_and_wait(&mut self, task: Task) -> anyhow::Result<WorkResult> {
        let mut result_rx = self.start()?;
        self.task_tx.send(task).await.expect("pool channel closed");
        // Drop the sender so workers know no more tasks are coming
        drop(self.task_tx.clone()); // Clone first since we need it for the struct
        Ok(result_rx.recv().await.expect("no result received"))
    }

    /// Submit multiple tasks and wait for all results.
    pub async fn submit_all_and_wait(
        &mut self,
        tasks: Vec<Task>,
    ) -> anyhow::Result<Vec<WorkResult>> {
        let count = tasks.len();
        let mut result_rx = self.start()?;

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
        Ok(results)
    }
}

/// A long-lived worker pool for the MCP server.
///
/// Unlike [`Pool`], `SharedPool` is started once and kept alive for the lifetime
/// of the server. Multiple callers can submit tasks concurrently — each gets back
/// its own result via a dedicated oneshot channel, so concurrent `kew_run` calls
/// from Claude actually execute in parallel across the pool's workers.
pub struct SharedPool {
    task_tx: mpsc::Sender<(Task, oneshot::Sender<WorkResult>)>,
}

impl SharedPool {
    /// Start a shared pool with `size` concurrent workers.
    /// Workers run as background tokio tasks and live until the pool is dropped.
    pub fn start(
        db: Database,
        ollama: Arc<dyn LlmClient>,
        claude: Option<Arc<dyn LlmClient>>,
        size: usize,
    ) -> Self {
        let (task_tx, task_rx) =
            mpsc::channel::<(Task, oneshot::Sender<WorkResult>)>(size * 16);
        let task_rx = Arc::new(tokio::sync::Mutex::new(task_rx));

        for i in 0..size {
            let worker_id = format!("worker-{i}");
            let db = db.clone();
            let ollama = ollama.clone();
            let claude = claude.clone();
            let task_rx = task_rx.clone();

            tokio::spawn(async move {
                let worker = Worker::new(worker_id.clone(), db, ollama, claude);
                debug!(worker = %worker_id, "shared worker started");

                loop {
                    let item = {
                        let mut rx = task_rx.lock().await;
                        rx.recv().await
                    };
                    match item {
                        Some((task, reply_tx)) => {
                            let result = worker.execute(&task).await;
                            let _ = reply_tx.send(result); // ignore if caller dropped
                        }
                        None => break, // pool dropped — shut down
                    }
                }

                debug!(worker = %worker_id, "shared worker stopped");
            });
        }

        // Watchdog: every 60s, find tasks stuck in 'running' with no log progress
        // for >5 minutes and mark them failed. This catches hung Ollama calls and
        // stalled tool loops — tool-calling agents write a log chunk on every LLM
        // iteration, so no log progress reliably means the worker is frozen.
        let watchdog_db = db.clone();
        tokio::spawn(async move {
            const IDLE_SECS: i64 = 300; // 5 minutes without log activity = stuck
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            interval.tick().await; // skip the immediate first tick
            loop {
                interval.tick().await;
                let conn = watchdog_db.conn();
                match crate::db::tasks::find_stuck_task_ids(&conn, IDLE_SECS) {
                    Ok(ids) if !ids.is_empty() => {
                        for id in &ids {
                            warn!(task_id = %id, "watchdog: task has had no log progress for >5m — marking failed");
                            let _ = crate::db::tasks::mark_failed(
                                &conn,
                                id,
                                "watchdog: no progress for 5 minutes (hung LLM call or stalled tool loop)",
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(e) => warn!("watchdog DB scan failed: {e}"),
                }
            }
        });

        info!(workers = size, "shared pool started");
        Self { task_tx }
    }

    /// Submit a task and await its result.
    /// Returns an error only if the pool has shut down or the task times out.
    ///
    /// Hard deadline: 10 minutes. The watchdog will mark stuck tasks failed at 5
    /// minutes of idle, but this timeout is a backstop for cases where the DB
    /// write itself is delayed or the worker crashes without sending a reply.
    pub async fn submit(&self, task: Task) -> anyhow::Result<WorkResult> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.task_tx
            .send((task, reply_tx))
            .await
            .map_err(|_| anyhow::anyhow!("shared pool channel closed"))?;
        tokio::time::timeout(Duration::from_secs(600), reply_rx)
            .await
            .map_err(|_| anyhow::anyhow!("task timed out after 10 minutes"))?
            .map_err(|_| anyhow::anyhow!("worker dropped reply before sending result"))
    }

    /// Submit a task and return immediately without waiting for the result.
    ///
    /// The task runs in the background. Use `kew_wait` with the task ID, or
    /// `kew_context_get` with the `share_as` key, to retrieve the result later.
    pub async fn submit_bg(&self, task: Task) -> anyhow::Result<()> {
        let (reply_tx, _reply_rx) = oneshot::channel(); // drop rx — fire and forget
        self.task_tx
            .send((task, reply_tx))
            .await
            .map_err(|_| anyhow::anyhow!("shared pool channel closed"))
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
                    message: ChatMessage::text("assistant", "done"),
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
            let claimed = tasks::claim_next_pending(&conn, &format!("w-{i}"))
                .unwrap()
                .unwrap();
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
        let results = pool.submit_all_and_wait(test_tasks).await.unwrap();

        assert_eq!(results.len(), 3);
        for r in &results {
            assert!(r.result.is_ok());
        }
    }
}
