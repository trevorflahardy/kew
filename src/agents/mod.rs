//! Pre-built agent configurations for common development workflows.
//!
//! Built-in agents are embedded in the binary at compile time. Users can override
//! any agent or add custom ones by placing YAML files in `.kew/agents/`.
//!
//! # Agent file format
//! ```yaml
//! name: my-agent
//! description: Short description shown in `kew agent list`
//! model: gemma4:26b        # optional preferred model
//! system_prompt: |
//!   You are a ...
//! ```

use anyhow::{Context, Result};
use serde::Deserialize;

/// A loaded agent configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub description: String,
    /// Preferred model. `kew run --agent` uses this unless `--model` is also given.
    pub model: Option<String>,
    pub system_prompt: String,
}

// Built-in agents embedded at compile time — zero runtime filesystem reads.
const BUILTIN_AGENTS: &[(&str, &str)] = &[
    ("developer", include_str!("builtin/developer.yaml")),
    ("debugger", include_str!("builtin/debugger.yaml")),
    ("docs-writer", include_str!("builtin/docs-writer.yaml")),
    ("security", include_str!("builtin/security.yaml")),
    ("doc-audit", include_str!("builtin/doc-audit.yaml")),
    ("tester", include_str!("builtin/tester.yaml")),
    ("watcher", include_str!("builtin/watcher.yaml")),
    ("error-finder", include_str!("builtin/error-finder.yaml")),
];

/// Load an agent by name.
///
/// Resolution order:
/// 1. `<project_dir>/.kew/agents/<name>.yaml` — project-local override
/// 2. `~/.config/kew/agents/<name>.yaml` — user-global override
/// 3. Built-in agents compiled into the binary
///
/// # Errors
/// Returns an error if the agent is not found or the YAML is malformed.
pub fn load_agent(name: &str, project_dir: Option<&std::path::Path>) -> Result<AgentConfig> {
    // 1. Project-local override
    if let Some(dir) = project_dir {
        let path = dir.join(".kew/agents").join(format!("{name}.yaml"));
        if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read agent file {path:?}"))?;
            return serde_yaml::from_str(&content)
                .with_context(|| format!("failed to parse agent '{name}' from {path:?}"));
        }
    }

    // 2. User-global override (~/.config/kew/agents/)
    if let Some(config_dir) = dirs_home_config() {
        let path = config_dir.join(format!("{name}.yaml"));
        if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read agent file {path:?}"))?;
            return serde_yaml::from_str(&content)
                .with_context(|| format!("failed to parse agent '{name}' from {path:?}"));
        }
    }

    // 3. Built-in
    for (builtin_name, yaml) in BUILTIN_AGENTS {
        if *builtin_name == name {
            return serde_yaml::from_str(yaml)
                .with_context(|| format!("failed to parse built-in agent '{name}'"));
        }
    }

    anyhow::bail!(
        "agent '{name}' not found.\n\
         Built-in agents: {}\n\
         Custom agents go in .kew/agents/{name}.yaml",
        BUILTIN_AGENTS
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// List all available agents: project-local, user-global, and built-in.
///
/// Returns `(name, description, source)` where source is one of
/// `"project"`, `"user"`, or `"builtin"`.
pub fn list_agents(project_dir: Option<&std::path::Path>) -> Vec<AgentEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut agents = Vec::new();

    // Project-local
    if let Some(dir) = project_dir {
        let agent_dir = dir.join(".kew/agents");
        collect_from_dir(&agent_dir, "project", &mut seen, &mut agents);
    }

    // User-global
    if let Some(config_dir) = dirs_home_config() {
        collect_from_dir(&config_dir, "user", &mut seen, &mut agents);
    }

    // Built-ins
    for (name, yaml) in BUILTIN_AGENTS {
        if seen.contains(*name) {
            continue; // overridden
        }
        if let Ok(cfg) = serde_yaml::from_str::<AgentConfig>(yaml) {
            agents.push(AgentEntry {
                name: name.to_string(),
                description: cfg.description,
                model: cfg.model,
                source: "builtin".into(),
            });
        }
    }

    agents
}

/// An entry in the agent listing.
#[derive(Debug, Clone)]
pub struct AgentEntry {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    /// `"builtin"`, `"project"`, or `"user"`
    pub source: String,
}

fn collect_from_dir(
    dir: &std::path::Path,
    source: &str,
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<AgentEntry>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
            continue;
        };
        if seen.contains(&name) {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(cfg) = serde_yaml::from_str::<AgentConfig>(&content) {
                seen.insert(name.clone());
                out.push(AgentEntry {
                    name,
                    description: cfg.description,
                    model: cfg.model,
                    source: source.into(),
                });
            }
        }
    }
}

fn dirs_home_config() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/kew/agents"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_builtins_parse() {
        for (name, yaml) in BUILTIN_AGENTS {
            let result = serde_yaml::from_str::<AgentConfig>(yaml);
            assert!(
                result.is_ok(),
                "built-in agent '{name}' failed to parse: {:?}",
                result.err()
            );
            let cfg = result.unwrap();
            assert_eq!(cfg.name, *name, "agent '{name}' has mismatched name field");
            assert!(
                !cfg.description.is_empty(),
                "agent '{name}' has empty description"
            );
            assert!(
                !cfg.system_prompt.is_empty(),
                "agent '{name}' has empty system_prompt"
            );
        }
    }

    #[test]
    fn test_load_builtin_developer() {
        let cfg = load_agent("developer", None).unwrap();
        assert_eq!(cfg.name, "developer");
        assert!(cfg.system_prompt.contains("senior software engineer"));
    }

    #[test]
    fn test_load_unknown_agent_errors() {
        let result = load_agent("nonexistent-agent-xyz", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_list_agents_includes_all_builtins() {
        let agents = list_agents(None);
        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        for (builtin_name, _) in BUILTIN_AGENTS {
            assert!(
                names.contains(builtin_name),
                "missing builtin '{builtin_name}' in list"
            );
        }
    }

    #[test]
    fn test_project_local_overrides_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join(".kew/agents");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("developer.yaml"),
            "name: developer\ndescription: custom\nsystem_prompt: custom prompt\n",
        )
        .unwrap();

        let cfg = load_agent("developer", Some(dir.path())).unwrap();
        assert_eq!(cfg.description, "custom");
        assert!(cfg.system_prompt.trim() == "custom prompt");
    }
}
