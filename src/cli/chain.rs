//! `kew chain` — execute a sequential chain of LLM tasks.
//!
//! Each step's output becomes context for the next step.
//! Example: kew chain --step "Analyze the code" --step "Write tests based on analysis"

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};

use crate::db::Database;
use crate::llm::ollama::OllamaClient;
use crate::llm::router;
use crate::worker::chain::{execute_chain, ChainStep};

#[derive(Args)]
pub struct ChainArgs {
    /// Chain step: "prompt" or "prompt:model" (repeatable)
    #[arg(short, long, required = true)]
    pub step: Vec<String>,

    /// Default model for steps without explicit model
    #[arg(short, long, default_value = "gemma4:26b")]
    pub model: String,

    /// System prompt (applied to all steps)
    #[arg(long)]
    pub system: Option<String>,

    /// Max wait time
    #[arg(long, default_value = "10m")]
    pub timeout: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// No spinner
    #[arg(short, long)]
    pub quiet: bool,
}

impl ChainArgs {
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
        Duration::from_secs(600)
    }

    /// Parse step specs. Format: "prompt" or "prompt:model".
    fn parse_steps(&self) -> Vec<ChainStep> {
        self.step
            .iter()
            .map(|s| {
                // Split on last ':' only if the part after looks like a model name
                // (contains '/' or no spaces). Otherwise treat whole thing as prompt.
                if let Some(colon_pos) = s.rfind(':') {
                    let (prompt, model_part) = s.split_at(colon_pos);
                    let model_candidate = &model_part[1..]; // skip ':'
                    if !model_candidate.is_empty()
                        && !model_candidate.contains(' ')
                    {
                        let route = router::route(model_candidate);
                        return ChainStep {
                            prompt: prompt.to_string(),
                            model: route.model,
                            provider: route.provider,
                            system_prompt: self.system.clone(),
                        };
                    }
                }
                let route = router::route(&self.model);
                ChainStep {
                    prompt: s.clone(),
                    model: route.model,
                    provider: route.provider,
                    system_prompt: self.system.clone(),
                }
            })
            .collect()
    }
}

pub async fn execute(args: &ChainArgs, db_path: &str, ollama_url: &str, claude_key: Option<&str>) -> Result<()> {
    let steps = args.parse_steps();
    if steps.is_empty() {
        anyhow::bail!("no steps provided");
    }

    let db = Database::open(std::path::Path::new(db_path))
        .context("failed to open database")?;
    let ollama: Arc<dyn crate::llm::LlmClient> = Arc::new(OllamaClient::new(ollama_url));
    let claude: Option<Arc<dyn crate::llm::LlmClient>> = claude_key
        .map(|key| Arc::new(crate::llm::claude::ClaudeClient::new(key)) as Arc<dyn crate::llm::LlmClient>);
    let chain_id = ulid::Ulid::new().to_string();

    let spinner = if !args.quiet && !args.json {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.cyan} {msg}")
                .expect("invalid template"),
        );
        pb.set_message(format!("chain: 0/{} steps...", steps.len()));
        pb.enable_steady_tick(Duration::from_millis(100));
        Some(pb)
    } else {
        None
    };

    let total_steps = steps.len();
    let timeout = args.parse_timeout();

    let result = tokio::time::timeout(timeout, async {
        execute_chain(&db, ollama, claude, steps, &chain_id).await
    })
    .await;

    if let Some(pb) = spinner {
        pb.finish_and_clear();
    }

    match result {
        Ok(results) => {
            if args.json {
                let json_results: Vec<serde_json::Value> = results
                    .iter()
                    .enumerate()
                    .map(|(i, r)| {
                        serde_json::json!({
                            "step": i,
                            "task_id": r.task_id,
                            "status": if r.result.is_ok() { "done" } else { "failed" },
                            "result": r.result.as_ref().ok(),
                            "error": r.result.as_ref().err(),
                            "duration_ms": r.stats.duration_ms,
                        })
                    })
                    .collect();
                let json = serde_json::json!({
                    "chain_id": chain_id,
                    "steps": total_steps,
                    "completed": results.len(),
                    "results": json_results,
                });
                println!("{}", serde_json::to_string_pretty(&json)?);
            } else {
                // Print final step's result (what the user usually wants)
                if let Some(last) = results.last() {
                    match &last.result {
                        Ok(text) => print!("{text}"),
                        Err(err) => anyhow::bail!("chain step {} failed: {err}", results.len() - 1),
                    }
                }
            }
        }
        Err(_) => {
            anyhow::bail!("chain timeout after {:?}", args.parse_timeout());
        }
    }

    Ok(())
}
