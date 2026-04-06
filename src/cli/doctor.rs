//! `kew doctor` — health check command.
//!
//! Verifies: Ollama is running, models are available, DB is writable.

use anyhow::Result;
use clap::Args;

use crate::llm::ollama::OllamaClient;
use crate::llm::LlmClient;

#[derive(Args)]
pub struct DoctorArgs {}

/// Execute the `kew doctor` command.
pub async fn execute(ollama_url: &str, db_path: &str) -> Result<()> {
    println!("kew doctor\n");
    let mut all_ok = true;

    // Check Ollama
    let client = OllamaClient::new(ollama_url);
    match client.ping().await {
        Ok(()) => {
            println!("\u{2713} Ollama is running at {ollama_url}");

            // List models
            match client.list_models().await {
                Ok(models) => {
                    if models.is_empty() {
                        println!("  \u{26a0} No models found. Run: ollama pull gemma3:27b");
                        all_ok = false;
                    } else {
                        println!("\u{2713} {} model(s) available:", models.len());
                        for m in &models {
                            println!("    - {m}");
                        }
                    }
                }
                Err(e) => {
                    println!("\u{2717} Failed to list models: {e}");
                    all_ok = false;
                }
            }
        }
        Err(e) => {
            println!("\u{2717} Ollama not reachable at {ollama_url}: {e}");
            println!("  Install: https://ollama.ai");
            all_ok = false;
        }
    }

    // Check database
    let db_path_p = std::path::Path::new(db_path);
    match crate::db::Database::open(db_path_p) {
        Ok(_db) => {
            println!("\u{2713} Database OK at {db_path}");
        }
        Err(e) => {
            println!("\u{2717} Database error: {e}");
            println!("  Run: kew init");
            all_ok = false;
        }
    }

    println!();
    if all_ok {
        println!("All checks passed.");
    } else {
        println!("Some checks failed. Fix the issues above.");
    }

    Ok(())
}
