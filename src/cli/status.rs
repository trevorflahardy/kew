//! `kew status` — show system status.
//!
//! With TUI feature: interactive ratatui dashboard.
//! Without: simple text summary.

use anyhow::{Context, Result};
use clap::Args;

use crate::db::{self, Database};

#[derive(Args)]
pub struct StatusArgs {
    /// Print summary and exit (no TUI)
    #[arg(long)]
    pub brief: bool,

    /// Machine-readable single-line output for status bars
    #[arg(long)]
    pub porcelain: bool,
}

pub fn execute(args: &StatusArgs, db_path: &str) -> Result<()> {
    let db = Database::open(std::path::Path::new(db_path)).context("failed to open database")?;
    let db_size_kb = db_size_kb(db_path);

    if args.porcelain {
        print_porcelain(&db, db_size_kb);
        return Ok(());
    }

    if args.brief || !cfg!(feature = "tui") {
        print_brief(&db, db_size_kb);
        return Ok(());
    }

    #[cfg(feature = "tui")]
    {
        crate::tui::dashboard::run(&db).map_err(|e| anyhow::anyhow!("TUI error: {e}"))?;
    }

    Ok(())
}

fn print_porcelain(db: &Database, db_size_kb: u64) {
    let conn = db.conn();
    let counts = db::tasks::count_by_status(&conn).unwrap_or_default();
    let get = |s: &str| {
        counts
            .iter()
            .find(|(k, _)| k == s)
            .map(|(_, v)| *v)
            .unwrap_or(0)
    };

    let context_count = db::context::list_context(&conn, None, 10000)
        .map(|v| v.len())
        .unwrap_or(0);
    let embedding_count = db::vectors::count_embeddings(&conn).unwrap_or(0);
    let (prompt_tokens, completion_tokens) =
        db::tasks::sum_tokens(&conn).unwrap_or((0, 0));

    println!(
        "pending={} running={} done={} failed={} context={} embeddings={} db={} prompt_tokens={} completion_tokens={}",
        get("pending"),
        get("running"),
        get("done"),
        get("failed"),
        context_count,
        embedding_count,
        format_size(db_size_kb),
        prompt_tokens,
        completion_tokens,
    );
}

fn print_brief(db: &Database, db_size_kb: u64) {
    let conn = db.conn();
    let counts = db::tasks::count_by_status(&conn).unwrap_or_default();
    let get = |s: &str| {
        counts
            .iter()
            .find(|(k, _)| k == s)
            .map(|(_, v)| *v)
            .unwrap_or(0)
    };

    println!("kew status\n");
    println!("  pending:  {}", get("pending"));
    println!("  running:  {}", get("running"));
    println!("  done:     {}", get("done"));
    println!("  failed:   {}", get("failed"));

    let context_count = db::context::list_context(&conn, None, 10000)
        .map(|v| v.len())
        .unwrap_or(0);
    let embedding_count = db::vectors::count_embeddings(&conn).unwrap_or(0);

    println!("\n  context entries: {context_count}");
    println!("  embeddings:     {embedding_count}");
    println!("  db size:        {}", format_size(db_size_kb));
}

/// Read file size in KB (0 if unavailable).
fn db_size_kb(path: &str) -> u64 {
    std::fs::metadata(path).map(|m| m.len() / 1024).unwrap_or(0)
}

/// Human-readable size: bytes → KB / MB / GB.
fn format_size(kb: u64) -> String {
    if kb == 0 {
        return "0KB".into();
    }
    if kb < 1024 {
        format!("{kb}KB")
    } else if kb < 1024 * 1024 {
        format!("{:.1}MB", kb as f64 / 1024.0)
    } else {
        format!("{:.2}GB", kb as f64 / (1024.0 * 1024.0))
    }
}
