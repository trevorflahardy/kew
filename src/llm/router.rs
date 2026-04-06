//! Model routing: maps model names to providers.

use crate::db::models::Provider;

/// Routing decision: which provider and model to use.
#[derive(Debug, Clone)]
pub struct RouteDecision {
    pub provider: Provider,
    pub model: String,
    pub reason: &'static str,
}

/// Route a model flag string to a provider.
///
/// Rules:
/// - `claude-*` prefix → Claude API
/// - Everything else → Ollama (local)
pub fn route(model: &str) -> RouteDecision {
    if model.starts_with("claude-") {
        RouteDecision {
            provider: Provider::Claude,
            model: model.to_string(),
            reason: "explicit Claude model prefix",
        }
    } else {
        RouteDecision {
            provider: Provider::Ollama,
            model: model.to_string(),
            reason: "local Ollama model",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ollama_routing() {
        let r = route("gemma4:26b");
        assert_eq!(r.provider, Provider::Ollama);
        assert_eq!(r.model, "gemma4:26b");
    }

    #[test]
    fn test_claude_routing() {
        let r = route("claude-sonnet-4-20250514");
        assert_eq!(r.provider, Provider::Claude);
        assert_eq!(r.model, "claude-sonnet-4-20250514");
    }

    #[test]
    fn test_unknown_defaults_to_ollama() {
        let r = route("codellama");
        assert_eq!(r.provider, Provider::Ollama);
    }
}
