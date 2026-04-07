//! `kew index` — embed project files for semantic search.
//!
//! Walks a directory, reads eligible files, embeds each via nomic-embed-text,
//! and stores both the embedding and raw content in the database.
//! After indexing, `kew_context_search` can find relevant source files by
//! natural-language query.
//!
//! With `--watch`, re-indexes files automatically as they change.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};

use crate::db::{self, Database};
use crate::llm::ollama::OllamaClient;
use crate::llm::LlmClient;

/// Default file extensions to index.
const DEFAULT_EXTENSIONS: &[&str] = &[
    "rs", "ts", "js", "tsx", "jsx", "go", "py", "md", "toml", "yaml", "yml", "json", "sql", "sh",
    "c", "cpp", "h",
];

/// Hard cap per file to avoid flooding the embedding model.
const FILE_SIZE_LIMIT: usize = 512 * 1024; // 512 KB

#[derive(Args)]
pub struct IndexArgs {
    /// Directory to index (default: current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Comma-separated file extensions to include (default: rs,ts,js,go,py,md,toml,yaml,json,sql,sh)
    #[arg(long)]
    pub ext: Option<String>,

    /// Re-embed files even if already indexed
    #[arg(long)]
    pub force: bool,

    /// Watch for file changes and re-index automatically after initial index
    #[arg(long)]
    pub watch: bool,

    /// Embedding model to use
    #[arg(long, default_value = "nomic-embed-text")]
    pub embed_model: String,
}

pub async fn execute(args: &IndexArgs, db_path: &str, ollama_url: &str) -> Result<()> {
    let db = Database::open(std::path::Path::new(db_path)).context("failed to open database")?;
    let ollama = Arc::new(OllamaClient::new(ollama_url));

    let root = args
        .path
        .canonicalize()
        .context("cannot resolve index path")?;

    let extensions: Vec<String> = args
        .ext
        .as_deref()
        .map(|s| s.split(',').map(|e| e.trim().to_lowercase()).collect())
        .unwrap_or_else(|| DEFAULT_EXTENSIONS.iter().map(|s| s.to_string()).collect());

    println!(
        "Indexing {} (extensions: {})",
        root.display(),
        extensions.join(", ")
    );

    let indexed = index_directory(
        &root,
        &root,
        &extensions,
        args.force,
        &db,
        &ollama,
        &args.embed_model,
    )
    .await?;
    println!("Indexed {indexed} files.");

    if args.watch {
        println!("Watching for changes (Ctrl-C to stop)...");
        watch_directory(&root, &extensions, &db, &ollama, &args.embed_model).await?;
    }

    Ok(())
}

/// Walk `dir`, embed eligible files, store in DB. Returns count of files indexed.
async fn index_directory(
    dir: &Path,
    root: &Path,
    extensions: &[String],
    force: bool,
    db: &Database,
    ollama: &Arc<OllamaClient>,
    embed_model: &str,
) -> Result<usize> {
    let files = collect_files(dir, extensions);

    let bar = ProgressBar::new(files.len() as u64);
    bar.set_style(
        ProgressStyle::default_bar()
            .template("{bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_bar()),
    );

    let mut count = 0;
    for path in &files {
        let rel = path.strip_prefix(root).unwrap_or(path);
        let key = format!("file:{}", rel.display());
        bar.set_message(rel.display().to_string());

        // Skip if already indexed and --force not set
        if !force {
            let conn = db.conn();
            if db::vectors::has_embedding(&conn, &key).unwrap_or(false) {
                bar.inc(1);
                continue;
            }
        }

        if let Err(e) = index_file(path, &key, root, db, ollama, embed_model).await {
            bar.println(format!("  skip {}: {e}", rel.display()));
        } else {
            count += 1;
        }
        bar.inc(1);
    }

    bar.finish_and_clear();
    Ok(count)
}

/// Embed a single file and store the embedding + content in the DB.
async fn index_file(
    path: &Path,
    key: &str,
    root: &Path,
    db: &Database,
    ollama: &Arc<OllamaClient>,
    embed_model: &str,
) -> Result<()> {
    let content = std::fs::read_to_string(path).context("read failed")?;

    // Truncate for embedding; store full content — stay on a char boundary
    let embed_text = if content.len() > FILE_SIZE_LIMIT {
        let mut boundary = FILE_SIZE_LIMIT;
        while boundary > 0 && !content.is_char_boundary(boundary) {
            boundary -= 1;
        }
        content[..boundary].to_string()
    } else {
        content.clone()
    };

    let vecs = ollama
        .embed(embed_model, &[embed_text])
        .await
        .context("embed failed")?;

    let embedding = vecs
        .into_iter()
        .next()
        .context("empty embedding response")?;

    let conn = db.conn();

    // Store embedding
    db::vectors::store_embedding(&conn, key, "file", None, &embedding, embed_model)
        .context("store embedding failed")?;

    // Store content in context table for retrieval
    let _rel = path.strip_prefix(root).unwrap_or(path);
    db::context::put_context(&conn, key, "file", &content, None).context("store context failed")?;

    drop(conn);
    Ok(())
}

/// Collect all eligible files under `dir` respecting .gitignore.
fn collect_files(dir: &Path, extensions: &[String]) -> Vec<PathBuf> {
    use ignore::WalkBuilder;

    WalkBuilder::new(dir)
        .hidden(false) // include dotfiles (e.g. .env.example)
        .git_ignore(true) // respect .gitignore
        .git_global(true) // respect global gitignore
        .ignore(true) // respect .ignore files
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter(|e| {
            let ext = e
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            extensions.iter().any(|allowed| allowed == &ext)
        })
        .map(|e| e.into_path())
        .collect()
}

/// Watch `root` for file changes and re-index on write/create events.
#[cfg(feature = "index")]
async fn watch_directory(
    root: &Path,
    extensions: &[String],
    db: &Database,
    ollama: &Arc<OllamaClient>,
    embed_model: &str,
) -> Result<()> {
    use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = RecommendedWatcher::new(tx, Config::default())?;
    watcher.watch(root, RecursiveMode::Recursive)?;

    let ext_set = extensions.to_vec();

    for event in rx {
        let Ok(event) = event else { continue };

        let is_write = matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_));
        if !is_write {
            continue;
        }

        for path in &event.paths {
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            if !ext_set.iter().any(|e| e == &ext) {
                continue;
            }
            if !path.is_file() {
                continue;
            }

            let rel = path.strip_prefix(root).unwrap_or(path);
            let key = format!("file:{}", rel.display());

            match index_file(path, &key, root, db, ollama, embed_model).await {
                Ok(()) => println!("re-indexed {}", rel.display()),
                Err(e) => eprintln!("failed to re-index {}: {e}", rel.display()),
            }
        }
    }

    Ok(())
}

/// Fallback when the `index` feature is disabled (no notify crate available).
#[cfg(not(feature = "index"))]
async fn watch_directory(
    _root: &Path,
    _extensions: &[String],
    _db: &Database,
    _ollama: &Arc<OllamaClient>,
    _embed_model: &str,
) -> Result<()> {
    anyhow::bail!("--watch requires the 'index' feature (rebuild with default features)")
}
