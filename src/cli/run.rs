//! `kew run` — execute a task through an LLM agent.
//!
//! This is the core command. `kew run --wait` is the primary integration
//! point for Claude Code: it blocks until the LLM returns, prints the
//! result to stdout, and exits.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};

use crate::db::models::NewTask;
use crate::db::{self, Database};
use crate::llm::ollama::OllamaClient;
use crate::llm::router;
use crate::worker::pool::Pool;

#[derive(Args)]
pub struct RunArgs {
    /// The prompt to send to the LLM
    #[arg()]
    pub prompt: Option<String>,

    /// Model name or alias
    #[arg(short, long, default_value = "gemma4:26b")]
    pub model: String,

    /// Block until complete, print result to stdout
    #[arg(short, long)]
    pub wait: bool,

    /// System prompt
    #[arg(short, long)]
    pub system: Option<String>,

    /// Read prompt from file
    #[arg(short, long)]
    pub file: Option<PathBuf>,

    /// Load context key (repeatable)
    #[arg(short, long)]
    pub context: Vec<String>,

    /// Store result as this context key
    #[arg(long)]
    pub share_as: Option<String>,

    /// File lock (repeatable)
    #[arg(long)]
    pub lock: Vec<String>,

    /// Max concurrent workers
    #[arg(short = 'n', long, default_value = "4")]
    pub workers: usize,

    /// Max wait time
    #[arg(long, default_value = "5m")]
    pub timeout: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// No spinner, just result
    #[arg(short, long)]
    pub quiet: bool,
}

impl RunArgs {
    /// Resolve the prompt from positional arg, --file, or stdin.
    fn resolve_prompt(&self) -> Result<String> {
        if let Some(ref prompt) = self.prompt {
            return Ok(prompt.clone());
        }

        if let Some(ref path) = self.file {
            return std::fs::read_to_string(path)
                .with_context(|| format!("reading prompt from {}", path.display()));
        }

        // Try reading from stdin if it's not a TTY
        if atty::is(atty::Stream::Stdin) {
            anyhow::bail!("no prompt provided. Pass a prompt as an argument, use --file, or pipe to stdin.");
        }

        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
        if buf.trim().is_empty() {
            anyhow::bail!("empty prompt from stdin");
        }
        Ok(buf)
    }

    /// Parse timeout string like "5m", "300s", "1h".
    fn parse_timeout(&self) -> Duration {
        let s = self.timeout.trim();
        if let Some(mins) = s.strip_suffix('m') {
            if let Ok(n) = mins.parse::<u64>() {
                return Duration::from_secs(n * 60);
            }
        }
        if let Some(secs) = s.strip_suffix('s') {
            if let Ok(n) = secs.parse::<u64>() {
                return Duration::from_secs(n);
            }
        }
        if let Some(hrs) = s.strip_suffix('h') {
            if let Ok(n) = hrs.parse::<u64>() {
                return Duration::from_secs(n * 3600);
            }
        }
        // Default 5 minutes
        Duration::from_secs(300)
    }
}

/// Execute the `kew run` command.
pub async fn execute(args: &RunArgs, db_path: &str, ollama_url: &str, claude_key: Option<&str>) -> Result<()> {
    let prompt = args.resolve_prompt()?;
    let route = router::route(&args.model);

    // Open DB
    let db = Database::open(std::path::Path::new(db_path))
        .context("failed to open database")?;

    // Create LLM clients
    let ollama: Arc<dyn crate::llm::LlmClient> = Arc::new(OllamaClient::new(ollama_url));
    let claude: Option<Arc<dyn crate::llm::LlmClient>> = claude_key
        .map(|key| Arc::new(crate::llm::claude::ClaudeClient::new(key)) as Arc<dyn crate::llm::LlmClient>);

    // Create the task
    let task = {
        let conn = db.conn();
        let new = NewTask {
            model: route.model.clone(),
            provider: route.provider.clone(),
            prompt: prompt.clone(),
            system_prompt: args.system.clone(),
            context_keys: args.context.clone(),
            share_as: args.share_as.clone(),
            files_locked: args.lock.clone(),
            parent_id: None,
            chain_id: None,
            chain_index: None,
        };
        db::tasks::create_task(&conn, &new)
            .context("failed to create task")?;
        db::tasks::claim_next_pending(&conn, "cli")
            .context("failed to claim task")?
            .expect("just-created task should be claimable")
    };

    if args.wait {
        // Show spinner unless quiet or JSON mode
        let spinner = if !args.quiet && !args.json {
            let pb = ProgressBar::new_spinner();
            pb.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.cyan} {msg}")
                    .expect("invalid template"),
            );
            pb.set_message(format!("running on {} via {}...", route.model, route.provider));
            pb.enable_steady_tick(Duration::from_millis(100));
            Some(pb)
        } else {
            None
        };

        // Execute with timeout
        let timeout = args.parse_timeout();
        let mut pool = Pool::new(db.clone(), ollama, claude, 1);
        let result = tokio::time::timeout(timeout, async {
            let results = pool.submit_all_and_wait(vec![task]).await;
            results.into_iter().next().expect("submitted 1 task")
        })
        .await;

        if let Some(pb) = spinner {
            pb.finish_and_clear();
        }

        match result {
            Ok(work_result) => match work_result.result {
                Ok(text) => {
                    if args.json {
                        let json = serde_json::json!({
                            "task_id": work_result.task_id,
                            "status": "done",
                            "model": route.model,
                            "result": text,
                            "duration_ms": work_result.stats.duration_ms,
                            "prompt_tokens": work_result.stats.prompt_tokens,
                            "completion_tokens": work_result.stats.completion_tokens,
                        });
                        println!("{}", serde_json::to_string_pretty(&json)?);
                    } else {
                        // Raw output — this is what Claude Code reads
                        print!("{text}");
                    }
                }
                Err(err) => {
                    if args.json {
                        let json = serde_json::json!({
                            "task_id": work_result.task_id,
                            "status": "failed",
                            "error": err,
                        });
                        println!("{}", serde_json::to_string_pretty(&json)?);
                    }
                    anyhow::bail!("task failed: {err}");
                }
            },
            Err(_) => {
                anyhow::bail!("timeout after {:?}", args.parse_timeout());
            }
        }
    } else {
        // Async mode: just print the task ID
        println!("{}", task.id);
    }

    Ok(())
}
