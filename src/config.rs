//! Project-level configuration loaded from `kew_config.yaml`.
//!
//! `KewConfig` is intentionally permissive — all fields are optional so that a
//! minimal or partially-written config file still loads without error. Callers
//! use the `defaults_*` helpers to get a resolved value with a sensible fallback.

use std::path::Path;

use serde::Deserialize;

/// Top-level structure of `kew_config.yaml`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct KewConfig {
    pub defaults: DefaultsConfig,
    pub ollama: OllamaConfig,
}

/// `defaults:` block — task execution defaults.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct DefaultsConfig {
    /// Default model name (e.g. "gemma4:26b").
    pub model: Option<String>,
    /// Number of concurrent workers in the pool.
    pub workers: Option<usize>,
    /// Task timeout as a human-readable string (e.g. "5m", "30s").
    pub timeout: Option<String>,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            model: None,
            workers: None,
            timeout: None,
        }
    }
}

/// `ollama:` block — Ollama connection settings.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct OllamaConfig {
    /// Ollama base URL (e.g. "http://localhost:11434").
    pub url: Option<String>,
    /// Model used for generating embeddings.
    pub embedding_model: Option<String>,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            url: None,
            embedding_model: None,
        }
    }
}

impl KewConfig {
    /// Load config from `kew_config.yaml` in `project_dir`.
    ///
    /// Returns `Ok(KewConfig::default())` if the file does not exist, so callers
    /// always get a usable config without special-casing a missing file.
    pub fn load(project_dir: &Path) -> anyhow::Result<Self> {
        let path = project_dir.join("kew_config.yaml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)?;
        let cfg: Self = serde_yaml::from_str(&content)?;
        Ok(cfg)
    }

    /// Load from the current working directory.
    pub fn load_cwd() -> anyhow::Result<Self> {
        let cwd = std::env::current_dir()?;
        Self::load(&cwd)
    }

    /// Resolved worker count: config value → fallback.
    pub fn workers(&self, fallback: usize) -> usize {
        self.defaults.workers.unwrap_or(fallback)
    }

    /// Resolved Ollama URL: config value → fallback.
    pub fn ollama_url<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.ollama.url.as_deref().unwrap_or(fallback)
    }

    /// Resolved default model: config value → fallback.
    pub fn model<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.defaults.model.as_deref().unwrap_or(fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_load_missing_file_returns_default() {
        let dir = tempdir().unwrap();
        let cfg = KewConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.workers(4), 4);
        assert_eq!(cfg.ollama_url("http://localhost:11434"), "http://localhost:11434");
    }

    #[test]
    fn test_load_full_config() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("kew_config.yaml"),
            r#"
defaults:
  model: gemma4:26b
  workers: 8
  timeout: 10m
ollama:
  url: http://myhost:11434
  embedding_model: nomic-embed-text
"#,
        )
        .unwrap();

        let cfg = KewConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.workers(4), 8);
        assert_eq!(cfg.ollama_url("http://localhost:11434"), "http://myhost:11434");
        assert_eq!(cfg.model("default"), "gemma4:26b");
    }

    #[test]
    fn test_load_partial_config_uses_fallbacks() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("kew_config.yaml"),
            "defaults:\n  workers: 2\n",
        )
        .unwrap();

        let cfg = KewConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.workers(4), 2);
        // ollama.url not set — falls back
        assert_eq!(cfg.ollama_url("http://localhost:11434"), "http://localhost:11434");
    }

    #[test]
    fn test_workers_fallback_when_not_set() {
        let cfg = KewConfig::default();
        assert_eq!(cfg.workers(4), 4);
        assert_eq!(cfg.workers(8), 8);
    }
}
