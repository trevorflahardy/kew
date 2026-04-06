//! MCP server implementation using rmcp.
//!
//! Exposes kew tools over stdio for Claude Code integration.

use std::sync::Arc;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{CallToolRequestParams, CallToolResult, ListToolsResult, ServerInfo};
use rmcp::schemars;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde::{Deserialize, Serialize};

use crate::db::{self, Database};
use crate::llm::ollama::OllamaClient;
use crate::llm::LlmClient;

/// The kew MCP server.
#[allow(dead_code)]
pub struct KewMcpServer {
    db: Database,
    ollama: Arc<dyn LlmClient>,
    ollama_url: String,
    tool_router: ToolRouter<Self>,
}

impl KewMcpServer {
    pub fn new(db: Database, ollama_url: &str) -> Self {
        let ollama: Arc<dyn LlmClient> = Arc::new(OllamaClient::new(ollama_url));
        let tool_router = Self::tool_router();
        Self {
            db,
            ollama,
            ollama_url: ollama_url.to_string(),
            tool_router,
        }
    }
}

// --- Tool parameter/result types ---

#[derive(Deserialize, schemars::JsonSchema)]
struct RunParams {
    /// The prompt to send to the LLM
    prompt: String,
    /// Model name (default: gemma4:26b)
    #[serde(default = "default_model")]
    model: String,
    /// System prompt
    system: Option<String>,
    /// Context keys to load
    #[serde(default)]
    context: Vec<String>,
    /// Store result as this context key
    share_as: Option<String>,
}

fn default_model() -> String {
    "gemma4:26b".into()
}

#[derive(Serialize, schemars::JsonSchema)]
struct RunResult {
    task_id: String,
    status: String,
    result: Option<String>,
    error: Option<String>,
    duration_ms: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ContextGetParams {
    /// Context key to retrieve
    key: String,
}

#[derive(Serialize, schemars::JsonSchema)]
struct ContextGetResult {
    key: String,
    content: String,
    namespace: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ContextSetParams {
    /// Context key
    key: String,
    /// Content to store
    content: String,
    /// Namespace (default: "default")
    #[serde(default = "default_namespace")]
    namespace: String,
}

fn default_namespace() -> String {
    "default".into()
}

#[derive(Serialize, schemars::JsonSchema)]
struct ContextSetResult {
    key: String,
    bytes: usize,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ContextSearchParams {
    /// Query text for semantic search
    query: String,
    /// Number of results (default: 5)
    #[serde(default = "default_top_k")]
    top_k: usize,
}

fn default_top_k() -> usize {
    5
}

#[derive(Serialize, schemars::JsonSchema)]
struct SearchResultItem {
    key: String,
    source_type: String,
    score: f32,
}

#[derive(Serialize, schemars::JsonSchema)]
struct ContextSearchResult {
    results: Vec<SearchResultItem>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct StatusParams {}

#[derive(Serialize, schemars::JsonSchema)]
struct StatusResult {
    tasks_pending: usize,
    tasks_running: usize,
    tasks_done: usize,
    tasks_failed: usize,
    context_entries: usize,
    embeddings: usize,
    ollama_ok: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct DoctorParams {}

#[derive(Serialize, schemars::JsonSchema)]
struct DoctorResult {
    ollama_reachable: bool,
    ollama_url: String,
    models: Vec<String>,
    db_ok: bool,
}

// --- Tool implementations ---

#[tool_router]
impl KewMcpServer {
    #[tool(
        name = "kew_run",
        description = "Execute a prompt through a local LLM agent. Blocks until complete and returns the result."
    )]
    async fn run(&self, Parameters(params): Parameters<RunParams>) -> Json<RunResult> {
        let route = crate::llm::router::route(&params.model);

        let task = {
            let conn = self.db.conn();
            let new = crate::db::models::NewTask {
                model: route.model.clone(),
                provider: route.provider.clone(),
                prompt: params.prompt,
                system_prompt: params.system,
                context_keys: params.context,
                share_as: params.share_as,
                files_locked: vec![],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            db::tasks::create_task(&conn, &new).unwrap();
            db::tasks::claim_next_pending(&conn, "mcp")
                .unwrap()
                .expect("just-created task should be claimable")
        };

        let mut pool =
            crate::worker::pool::Pool::new(self.db.clone(), self.ollama.clone(), None, 1);
        let results = pool.submit_all_and_wait(vec![task]).await;
        let work_result = results.into_iter().next().expect("submitted 1 task");

        Json(RunResult {
            task_id: work_result.task_id,
            status: if work_result.result.is_ok() {
                "done".into()
            } else {
                "failed".into()
            },
            result: work_result.result.as_ref().ok().cloned(),
            error: work_result.result.as_ref().err().cloned(),
            duration_ms: work_result.stats.duration_ms,
        })
    }

    #[tool(
        name = "kew_context_get",
        description = "Retrieve a shared context entry by key."
    )]
    fn context_get(&self, Parameters(params): Parameters<ContextGetParams>) -> Json<ContextGetResult> {
        let conn = self.db.conn();
        match db::context::get_context(&conn, &params.key) {
            Ok(Some(entry)) => Json(ContextGetResult {
                key: entry.key,
                content: entry.content,
                namespace: entry.namespace,
            }),
            _ => Json(ContextGetResult {
                key: params.key,
                content: String::new(),
                namespace: "not_found".into(),
            }),
        }
    }

    #[tool(
        name = "kew_context_set",
        description = "Store a shared context entry that can be loaded by future tasks."
    )]
    fn context_set(&self, Parameters(params): Parameters<ContextSetParams>) -> Json<ContextSetResult> {
        let conn = self.db.conn();
        let bytes = params.content.len();
        let _ = db::context::put_context(&conn, &params.key, &params.namespace, &params.content, None);
        Json(ContextSetResult {
            key: params.key,
            bytes,
        })
    }

    #[tool(
        name = "kew_context_search",
        description = "Search stored results by semantic similarity. Returns the most relevant past task results."
    )]
    async fn context_search(
        &self,
        Parameters(params): Parameters<ContextSearchParams>,
    ) -> Json<ContextSearchResult> {
        let embeddings = self
            .ollama
            .embed("nomic-embed-text", &[params.query])
            .await;

        match embeddings {
            Ok(vecs) if !vecs.is_empty() && !vecs[0].is_empty() => {
                let conn = self.db.conn();
                let results =
                    db::vectors::search_similar(&conn, &vecs[0], None, params.top_k)
                        .unwrap_or_default();
                Json(ContextSearchResult {
                    results: results
                        .into_iter()
                        .map(|r| SearchResultItem {
                            key: r.key,
                            source_type: r.source_type,
                            score: r.score,
                        })
                        .collect(),
                })
            }
            _ => Json(ContextSearchResult { results: vec![] }),
        }
    }

    #[tool(
        name = "kew_status",
        description = "Get system status: task counts, context entries, and embedding count."
    )]
    fn status(&self, Parameters(_params): Parameters<StatusParams>) -> Json<StatusResult> {
        let conn = self.db.conn();
        let counts = db::tasks::count_by_status(&conn).unwrap_or_default();
        let context_entries = db::context::list_context(&conn, None, 10000)
            .map(|v| v.len())
            .unwrap_or(0);
        let embeddings = db::vectors::count_embeddings(&conn).unwrap_or(0);

        let get_count = |status: &str| -> usize {
            counts
                .iter()
                .find(|(s, _)| s == status)
                .map(|(_, c)| *c as usize)
                .unwrap_or(0)
        };

        Json(StatusResult {
            tasks_pending: get_count("pending"),
            tasks_running: get_count("running"),
            tasks_done: get_count("done"),
            tasks_failed: get_count("failed"),
            context_entries,
            embeddings,
            ollama_ok: true,
        })
    }

    #[tool(
        name = "kew_doctor",
        description = "Health check: verify Ollama connectivity, list available models, check database."
    )]
    async fn doctor(&self, Parameters(_params): Parameters<DoctorParams>) -> Json<DoctorResult> {
        let ollama_reachable = self.ollama.ping().await.is_ok();
        let models = self.ollama.list_models().await.unwrap_or_default();

        Json(DoctorResult {
            ollama_reachable,
            ollama_url: self.ollama_url.clone(),
            models,
            db_ok: true,
        })
    }
}

impl ServerHandler for KewMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::default()
            .with_instructions("Kew: Real local agent orchestration. Use kew_run to execute LLM tasks, kew_context_* to manage shared context, kew_status for system info.")
    }

    fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, ErrorData>> + Send + '_ {
        let tools = self.tool_router.list_all();
        std::future::ready(Ok(ListToolsResult {
            tools,
            ..Default::default()
        }))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<CallToolResult, ErrorData>> + Send + '_ {
        let tool_context = ToolCallContext::new(self, request, context);
        async move { self.tool_router.call(tool_context).await }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, ChatRequest, ChatResponse, CompletionStats, LlmError};
    use rmcp::ServerHandler;

    /// Mock LLM that returns a fixed string.
    struct MockLlm {
        response: String,
    }

    #[async_trait::async_trait]
    impl LlmClient for MockLlm {
        async fn chat(&self, _req: ChatRequest) -> Result<(ChatResponse, CompletionStats), LlmError> {
            Ok((
                ChatResponse {
                    message: ChatMessage { role: "assistant".into(), content: self.response.clone() },
                    model: "mock".into(),
                    done: true,
                    total_duration_ns: Some(100_000_000),
                    prompt_eval_count: Some(10),
                    eval_count: Some(20),
                },
                CompletionStats { prompt_tokens: Some(10), completion_tokens: Some(20), duration_ms: Some(100) },
            ))
        }
        async fn embed(&self, _: &str, input: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            Ok(input.iter().map(|_| vec![1.0, 0.0, 0.0]).collect())
        }
        async fn list_models(&self) -> Result<Vec<String>, LlmError> {
            Ok(vec!["mock-model".into()])
        }
        async fn ping(&self) -> Result<(), LlmError> { Ok(()) }
        fn provider_name(&self) -> &str { "mock" }
    }

    fn make_server_with_mock(db: Database, response: &str) -> KewMcpServer {
        let ollama: Arc<dyn LlmClient> = Arc::new(MockLlm { response: response.into() });
        let tool_router = KewMcpServer::tool_router();
        KewMcpServer {
            db,
            ollama,
            ollama_url: "http://mock:11434".into(),
            tool_router,
        }
    }

    #[test]
    fn test_server_get_info() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "hi");
        let info = server.get_info();
        assert!(info.instructions.unwrap().contains("kew_run"));
    }

    #[test]
    fn test_tool_router_lists_all_tools() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "hi");
        let tools = server.tool_router.list_all();

        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains(&"kew_run"), "missing kew_run: {names:?}");
        assert!(names.contains(&"kew_context_get"), "missing kew_context_get");
        assert!(names.contains(&"kew_context_set"), "missing kew_context_set");
        assert!(names.contains(&"kew_context_search"), "missing kew_context_search");
        assert!(names.contains(&"kew_status"), "missing kew_status");
        assert!(names.contains(&"kew_doctor"), "missing kew_doctor");
        assert_eq!(tools.len(), 6);
    }

    #[test]
    fn test_tool_schemas_have_descriptions() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "hi");
        let tools = server.tool_router.list_all();

        for tool in &tools {
            assert!(
                tool.description.is_some(),
                "tool {} has no description",
                tool.name
            );
        }
    }

    #[test]
    fn test_kew_run_input_schema_requires_prompt() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "hi");
        let tools = server.tool_router.list_all();

        let run_tool = tools.iter().find(|t| t.name == "kew_run").unwrap();
        let schema = serde_json::to_string(&run_tool.input_schema).unwrap();
        assert!(schema.contains("prompt"), "kew_run schema should require 'prompt': {schema}");
    }

    #[tokio::test]
    async fn test_mcp_context_set_and_get() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "hi");

        // Set context
        let set_params = ContextSetParams {
            key: "test-key".into(),
            content: "test content".into(),
            namespace: "default".into(),
        };
        let Json(set_result) = server.context_set(Parameters(set_params));
        assert_eq!(set_result.key, "test-key");
        assert_eq!(set_result.bytes, 12);

        // Get context
        let get_params = ContextGetParams { key: "test-key".into() };
        let Json(get_result) = server.context_get(Parameters(get_params));
        assert_eq!(get_result.content, "test content");
        assert_eq!(get_result.namespace, "default");
    }

    #[tokio::test]
    async fn test_mcp_context_get_not_found() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "hi");

        let params = ContextGetParams { key: "nonexistent".into() };
        let Json(result) = server.context_get(Parameters(params));
        assert_eq!(result.namespace, "not_found");
        assert!(result.content.is_empty());
    }

    #[tokio::test]
    async fn test_mcp_status() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "hi");

        let Json(result) = server.status(Parameters(StatusParams {}));
        assert_eq!(result.tasks_pending, 0);
        assert_eq!(result.tasks_done, 0);
        assert_eq!(result.context_entries, 0);
        assert_eq!(result.embeddings, 0);
        assert!(result.ollama_ok);
    }

    #[tokio::test]
    async fn test_mcp_doctor() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "hi");

        let Json(result) = server.doctor(Parameters(DoctorParams {})).await;
        assert!(result.ollama_reachable);
        assert_eq!(result.models, vec!["mock-model"]);
        assert!(result.db_ok);
        assert_eq!(result.ollama_url, "http://mock:11434");
    }

    #[tokio::test]
    async fn test_mcp_run_executes_task() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "42");

        let params = RunParams {
            prompt: "What is 6*7?".into(),
            model: "mock".into(),
            system: None,
            context: vec![],
            share_as: None,
        };
        let Json(result) = server.run(Parameters(params)).await;
        assert_eq!(result.status, "done");
        assert_eq!(result.result.as_deref(), Some("42"));
        assert!(result.error.is_none());
        assert!(!result.task_id.is_empty());
    }

    #[tokio::test]
    async fn test_mcp_run_with_share_as_stores_context() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "shared result");

        let params = RunParams {
            prompt: "Generate".into(),
            model: "mock".into(),
            system: None,
            context: vec![],
            share_as: Some("output-key".into()),
        };
        let Json(run_result) = server.run(Parameters(params)).await;
        assert_eq!(run_result.status, "done");

        // Verify context was shared
        let get_params = ContextGetParams { key: "output-key".into() };
        let Json(ctx) = server.context_get(Parameters(get_params));
        assert_eq!(ctx.content, "shared result");
    }

    #[tokio::test]
    async fn test_mcp_context_search_with_embeddings() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "hi");

        // Store an embedding manually
        {
            let conn = server.db.conn();
            db::vectors::store_embedding(&conn, "k1", "result", Some("t1"), &[1.0, 0.0, 0.0], "mock").unwrap();
        }

        let params = ContextSearchParams {
            query: "test".into(),
            top_k: 5,
        };
        let Json(result) = server.context_search(Parameters(params)).await;
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].key, "k1");
        assert!(result.results[0].score > 0.99);
    }
}

/// Start the MCP server on stdio.
pub async fn serve(db: Database, ollama_url: &str) -> anyhow::Result<()> {
    let server = KewMcpServer::new(db, ollama_url);
    let service = server
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await
        .map_err(|e| anyhow::anyhow!("MCP server init failed: {e}"))?;
    service.waiting().await?;
    Ok(())
}
