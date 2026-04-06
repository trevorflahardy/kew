//! `kew context` — manage shared context entries.
//!
//! Subcommands: list, get, set, delete, search, clear.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use crate::db::{self, Database};
use crate::llm::ollama::OllamaClient;
use crate::llm::LlmClient;

#[derive(Args)]
pub struct ContextArgs {
    #[command(subcommand)]
    pub command: ContextCommands,
}

#[derive(Subcommand)]
pub enum ContextCommands {
    /// List all context entries
    List {
        /// Filter by namespace
        #[arg(short, long)]
        namespace: Option<String>,

        /// Max entries to show
        #[arg(short, long, default_value = "50")]
        limit: usize,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Get a context entry by key
    Get {
        /// Context key
        key: String,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Set a context entry
    Set {
        /// Context key
        key: String,

        /// Content (or read from stdin if omitted)
        content: Option<String>,

        /// Namespace
        #[arg(short, long, default_value = "default")]
        namespace: String,
    },

    /// Delete a context entry
    Delete {
        /// Context key
        key: String,
    },

    /// Search context by vector similarity
    Search {
        /// Query text to search for
        query: String,

        /// Number of results
        #[arg(short, long, default_value = "5")]
        top_k: usize,

        /// Embedding model
        #[arg(long, default_value = "nomic-embed-text")]
        embed_model: String,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Clear all context entries
    Clear {
        /// Only clear this namespace
        #[arg(short, long)]
        namespace: Option<String>,

        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },
}

pub async fn execute(args: &ContextArgs, db_path: &str, ollama_url: &str) -> Result<()> {
    let db = Database::open(std::path::Path::new(db_path)).context("failed to open database")?;

    match &args.command {
        ContextCommands::List {
            namespace,
            limit,
            json,
        } => {
            let conn = db.conn();
            let entries = db::context::list_context(&conn, namespace.as_deref(), *limit)
                .context("failed to list context")?;

            if *json {
                let json_val: Vec<serde_json::Value> = entries
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "key": e.key,
                            "namespace": e.namespace,
                            "content_length": e.content.len(),
                            "created_by": e.created_by,
                            "updated_at": e.updated_at,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&json_val)?);
            } else if entries.is_empty() {
                println!("No context entries.");
            } else {
                for entry in &entries {
                    let by = entry.created_by.as_deref().unwrap_or("manual");
                    println!(
                        "  {} [{}] ({} bytes, by {})",
                        entry.key,
                        entry.namespace,
                        entry.content.len(),
                        by
                    );
                }
            }
        }

        ContextCommands::Get { key, json } => {
            let conn = db.conn();
            let entry = db::context::get_context(&conn, key).context("failed to get context")?;

            match entry {
                Some(e) => {
                    if *json {
                        let json_val = serde_json::json!({
                            "key": e.key,
                            "namespace": e.namespace,
                            "content": e.content,
                            "created_by": e.created_by,
                            "updated_at": e.updated_at,
                        });
                        println!("{}", serde_json::to_string_pretty(&json_val)?);
                    } else {
                        print!("{}", e.content);
                    }
                }
                None => {
                    anyhow::bail!("context key '{key}' not found");
                }
            }
        }

        ContextCommands::Set {
            key,
            content,
            namespace,
        } => {
            let text = match content {
                Some(c) => c.clone(),
                None => {
                    if atty::is(atty::Stream::Stdin) {
                        anyhow::bail!(
                            "no content provided. Pass content as argument or pipe to stdin."
                        );
                    }
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                    buf
                }
            };

            let conn = db.conn();
            db::context::put_context(&conn, key, namespace, &text, None)
                .context("failed to set context")?;
            eprintln!("set context '{key}' ({} bytes)", text.len());
        }

        ContextCommands::Search {
            query,
            top_k,
            embed_model,
            json,
        } => {
            let client = OllamaClient::new(ollama_url);
            let embeddings = client
                .embed(embed_model, std::slice::from_ref(query))
                .await
                .context("failed to generate query embedding")?;

            if embeddings.is_empty() || embeddings[0].is_empty() {
                anyhow::bail!("embedding model returned empty vector");
            }

            let conn = db.conn();
            let results = db::vectors::search_similar(&conn, &embeddings[0], None, *top_k)
                .context("failed to search embeddings")?;

            if *json {
                let json_results: Vec<serde_json::Value> = results
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "key": r.key,
                            "source_type": r.source_type,
                            "source_id": r.source_id,
                            "score": r.score,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&json_results)?);
            } else if results.is_empty() {
                println!("No similar entries found.");
            } else {
                for r in &results {
                    let sid = r.source_id.as_deref().unwrap_or("-");
                    println!("  {:.4}  {} [{}] ({})", r.score, r.key, r.source_type, sid);
                }
            }
        }

        ContextCommands::Delete { key } => {
            let conn = db.conn();
            let deleted =
                db::context::delete_context(&conn, key).context("failed to delete context")?;
            if deleted {
                eprintln!("deleted context '{key}'");
            } else {
                anyhow::bail!("context key '{key}' not found");
            }
        }

        ContextCommands::Clear { namespace, force } => {
            if !force {
                let scope = namespace.as_deref().unwrap_or("all namespaces");
                eprintln!("This will delete all context entries in {scope}.");
                eprintln!("Use --force to skip this warning.");
                anyhow::bail!("aborted (use --force)");
            }

            let conn = db.conn();
            let count = db::context::clear_context(&conn, namespace.as_deref())
                .context("failed to clear context")?;
            eprintln!("cleared {count} context entries");
        }
    }

    Ok(())
}
