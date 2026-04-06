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
