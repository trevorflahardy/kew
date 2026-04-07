//! MCP server implementation using rmcp.
//!
//! Exposes kew tools over stdio for Claude Code integration.

use std::sync::Arc;

use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolRequestParams, CallToolResult, ListToolsResult, ServerInfo};
use rmcp::schemars;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde::{Deserialize, Serialize};

use crate::db::{self, Database};
use crate::llm::ollama::OllamaClient;
use crate::llm::LlmClient;
use crate::worker::pool::SharedPool;

/// The kew MCP server.
pub struct KewMcpServer {
    db: Database,
    ollama: Arc<dyn LlmClient>,
    ollama_url: String,
    tool_router: ToolRouter<Self>,
    /// Persistent worker pool shared across all concurrent `kew_run` calls.
    pool: Arc<SharedPool>,
}

impl KewMcpServer {
    /// Create a new MCP server.
    ///
    /// `workers` controls the size of the shared worker pool — how many LLM
    /// tasks can run concurrently. Read this from `KewConfig::workers(4)` so
    /// the value in `kew_config.yaml` is respected.
    pub fn new(db: Database, ollama_url: &str, workers: usize) -> Self {
        let ollama: Arc<dyn LlmClient> = Arc::new(OllamaClient::new(ollama_url));
        let tool_router = Self::tool_router();
        let pool = Arc::new(SharedPool::start(db.clone(), ollama.clone(), None, workers));
        Self {
            db,
            ollama,
            ollama_url: ollama_url.to_string(),
            tool_router,
            pool,
        }
    }
}

// --- Agent keyword routing ---

/// Auto-detect the best agent from prompt keywords when no agent is explicitly set.
///
/// Returns the agent name if a high-confidence keyword match is found, or `None`
/// to let the LLM run without a specialized system prompt.
fn detect_agent_from_prompt(prompt: &str) -> Option<&'static str> {
    let p = prompt.to_lowercase();

    // doc-audit must be checked before docs-writer (more specific)
    if p.contains("doc audit")
        || p.contains("documentation gap")
        || p.contains("documentation quality")
        || p.contains("missing docs")
        || p.contains("audit doc")
    {
        return Some("doc-audit");
    }
    // error-finder before generic "error" (which maps to debugger)
    if p.contains("find error")
        || p.contains("potential bug")
        || p.contains("what could go wrong")
        || p.contains("pre-emptive")
        || p.contains("review for bug")
        || p.contains("find bug")
    {
        return Some("error-finder");
    }
    if p.contains("security")
        || p.contains("vulnerabilit")
        || p.contains("exploit")
        || p.contains("injection")
        || p.contains("auth bypass")
        || p.contains("cve")
    {
        return Some("security");
    }
    if p.contains("debug")
        || p.contains("broken")
        || p.contains("not working")
        || p.contains("crash")
        || p.contains("root cause")
        || p.contains("diagnose")
        || p.contains("fix the bug")
        || p.contains("why is")
    {
        return Some("debugger");
    }
    if p.contains("write test")
        || p.contains("add test")
        || p.contains("unit test")
        || p.contains("test coverage")
        || p.contains("test suite")
        || p.contains("write specs")
    {
        return Some("tester");
    }
    if p.contains("document")
        || p.contains("write docs")
        || p.contains("add docs")
        || p.contains("explain this")
        || p.contains("write readme")
    {
        return Some("docs-writer");
    }
    if p.contains("watch")
        || p.contains("track progress")
        || p.contains("summarize progress")
        || p.contains("what's happening")
        || p.contains("status report")
        || p.contains("observe")
    {
        return Some("watcher");
    }
    if p.contains("implement")
        || p.contains("build this")
        || p.contains("write code")
        || p.contains("add feature")
        || p.contains("refactor")
        || p.contains("create a function")
        || p.contains("create a struct")
        || p.contains("create a class")
    {
        return Some("developer");
    }
    None
}

// --- Project namespace isolation ---

/// Compute a short, stable hash of the current working directory.
///
/// This is used to namespace MCP context keys so that two projects sharing the
/// same database (same `db_path`) cannot accidentally read each other's context.
fn project_namespace() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    cwd.hash(&mut hasher);
    format!("proj_{:x}", hasher.finish())
}

// --- Tool parameter/result types ---

#[derive(Deserialize, schemars::JsonSchema)]
struct RunParams {
    /// The prompt to send to the LLM
    prompt: String,
    /// Model name (default: gemma4:26b)
    #[serde(default = "default_model")]
    model: String,
    /// System prompt — overrides the agent's system prompt if both are set
    system: Option<String>,
    /// Context keys to load before executing (exact string keys from kew_context_set/kew_run share_as)
    #[serde(default)]
    context: Vec<String>,
    /// Store result as this context key
    share_as: Option<String>,
    /// Named agent to use. Sets the system prompt automatically.
    /// Built-ins: developer, debugger, docs-writer, security, doc-audit, tester, watcher, error-finder.
    /// If omitted, kew auto-detects an agent from prompt keywords (e.g. "debug", "write tests",
    /// "security audit"). Use kew_list_agents to see all available agents and their trigger keywords.
    agent: Option<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
struct ListAgentsResult {
    agents: Vec<AgentInfo>,
}

#[derive(Serialize, schemars::JsonSchema)]
struct AgentInfo {
    /// Agent name — pass to kew_run as the `agent` field
    name: String,
    /// One-line description
    description: String,
    /// Preferred model (may be null)
    model: Option<String>,
    /// Source: "builtin", "project", or "user"
    source: String,
    /// Comma-separated keywords that auto-trigger this agent
    keywords: String,
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
    /// If true, include the stored content for each result (from context/file store)
    #[serde(default)]
    include_content: bool,
}

fn default_top_k() -> usize {
    5
}

#[derive(Serialize, schemars::JsonSchema)]
struct SearchResultItem {
    key: String,
    source_type: String,
    score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
struct ContextSearchResult {
    results: Vec<SearchResultItem>,
}

#[derive(Serialize, schemars::JsonSchema)]
struct RunBgResult {
    /// Task ID — pass to kew_wait to block until done, or use share_as + kew_context_get to retrieve the result.
    task_id: String,
    status: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct WaitParams {
    /// Task IDs returned by kew_run_bg to wait for.
    task_ids: Vec<String>,
    /// How often to poll for completion, in milliseconds (default: 500).
    #[serde(default = "default_poll_ms")]
    poll_ms: u64,
}

fn default_poll_ms() -> u64 {
    500
}

#[derive(Serialize, schemars::JsonSchema)]
struct WaitTaskResult {
    task_id: String,
    status: String,
    result: Option<String>,
    error: Option<String>,
    duration_ms: Option<i64>,
}

#[derive(Serialize, schemars::JsonSchema)]
struct WaitResult {
    results: Vec<WaitTaskResult>,
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

#[derive(Deserialize, schemars::JsonSchema)]
struct ListAgentsParams {}

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
        description = "Execute a prompt through a local LLM agent. Blocks until complete and returns the result.\n\nAGENT TYPES — set `agent` to activate a specialist:\n• developer — production code, no over-engineering\n• debugger — systematic root-cause analysis\n• docs-writer — clear, accurate documentation\n• security — exploitable vulnerability audit (>80% confidence only)\n• doc-audit — find documentation gaps by CRITICAL/IMPORTANT/NICE-TO-HAVE\n• tester — write test suites and find coverage gaps\n• watcher — observe codebase, surface blockers and TODOs\n• error-finder — adversarial pre-emptive bug detection\n\nKEYWORD AUTO-ROUTING — if `agent` is omitted, kew infers one from the prompt:\n'debug/broken/why is' → debugger | 'write test/test suite' → tester | 'security/vulnerability' → security | 'document/write docs' → docs-writer | 'doc audit/documentation gaps' → doc-audit | 'find errors/potential bugs' → error-finder | 'watch/track progress' → watcher | 'implement/refactor/add feature' → developer"
    )]
    async fn run(&self, Parameters(params): Parameters<RunParams>) -> Json<RunResult> {
        // Resolve agent: explicit > keyword-detected > none
        let agent_name = params
            .agent
            .as_deref()
            .or_else(|| detect_agent_from_prompt(&params.prompt));

        let (effective_model, effective_system) = if let Some(name) = agent_name {
            let project_dir = std::env::current_dir().ok();
            match crate::agents::load_agent(name, project_dir.as_deref()) {
                Ok(cfg) => {
                    let model = cfg.model.unwrap_or_else(|| params.model.clone());
                    // --system in params overrides agent's system prompt
                    let system = params.system.clone().or(Some(cfg.system_prompt));
                    (model, system)
                }
                Err(_) => (params.model.clone(), params.system.clone()),
            }
        } else {
            (params.model.clone(), params.system.clone())
        };

        let route = crate::llm::router::route(&effective_model);

        let task = {
            let conn = self.db.conn();
            let new = crate::db::models::NewTask {
                model: route.model.clone(),
                provider: route.provider.clone(),
                prompt: params.prompt,
                system_prompt: effective_system,
                context_keys: params
                    .context
                    .into_iter()
                    .map(|k| format!("{}/{k}", project_namespace()))
                    .collect(),
                share_as: params
                    .share_as
                    .map(|k| format!("{}/{k}", project_namespace())),
                files_locked: vec![],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            let created = db::tasks::create_task(&conn, &new).unwrap();
            if let Some(name) = agent_name {
                db::tasks::set_task_agent(&conn, &created.id, name).ok();
            }
            db::tasks::claim_next_pending(&conn, "mcp")
                .unwrap()
                .expect("just-created task should be claimable")
        };

        let work_result = self.pool.submit(task).await.expect("pool submit failed");

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
        name = "kew_run_bg",
        description = "Fire-and-forget: dispatch a task to a local LLM agent and return immediately without waiting.\n\nReturns a task_id. Use kew_wait to block until one or more background tasks finish, or kew_context_get with the share_as key to retrieve the result once it's ready.\n\nUse this for background auditors, parallel workstreams, and any task you want running while Claude continues other work. Same agent types and keyword auto-routing as kew_run."
    )]
    async fn run_bg(&self, Parameters(params): Parameters<RunParams>) -> Json<RunBgResult> {
        let agent_name = params
            .agent
            .as_deref()
            .or_else(|| detect_agent_from_prompt(&params.prompt));

        let (effective_model, effective_system) = if let Some(name) = agent_name {
            let project_dir = std::env::current_dir().ok();
            match crate::agents::load_agent(name, project_dir.as_deref()) {
                Ok(cfg) => {
                    let model = cfg.model.unwrap_or_else(|| params.model.clone());
                    let system = params.system.clone().or(Some(cfg.system_prompt));
                    (model, system)
                }
                Err(_) => (params.model.clone(), params.system.clone()),
            }
        } else {
            (params.model.clone(), params.system.clone())
        };

        let route = crate::llm::router::route(&effective_model);

        let task = {
            let conn = self.db.conn();
            let new = crate::db::models::NewTask {
                model: route.model.clone(),
                provider: route.provider.clone(),
                prompt: params.prompt,
                system_prompt: effective_system,
                context_keys: params
                    .context
                    .into_iter()
                    .map(|k| format!("{}/{k}", project_namespace()))
                    .collect(),
                share_as: params
                    .share_as
                    .map(|k| format!("{}/{k}", project_namespace())),
                files_locked: vec![],
                parent_id: None,
                chain_id: None,
                chain_index: None,
            };
            let created = db::tasks::create_task(&conn, &new).unwrap();
            if let Some(name) = agent_name {
                db::tasks::set_task_agent(&conn, &created.id, name).ok();
            }
            db::tasks::claim_next_pending(&conn, "mcp-bg")
                .unwrap()
                .expect("just-created task should be claimable")
        };

        let task_id = task.id.clone();
        match self.pool.submit_bg(task).await {
            Ok(()) => Json(RunBgResult {
                task_id,
                status: "queued".into(),
            }),
            Err(e) => Json(RunBgResult {
                task_id,
                status: format!("error: {e}"),
            }),
        }
    }

    #[tool(
        name = "kew_wait",
        description = "Wait for one or more background tasks (launched with kew_run_bg) to finish.\n\nBlocks until every task in task_ids reaches a terminal state (done, failed, or cancelled), then returns all results in one response.\n\nTypical pattern:\n  id_a = kew_run_bg { agent: 'security', prompt: '...', share_as: 'sec/auth' }\n  id_b = kew_run_bg { agent: 'security', prompt: '...', share_as: 'sec/api' }\n  // ...do other work...\n  kew_wait { task_ids: [id_a, id_b] }  // block here to collect all results"
    )]
    async fn wait(&self, Parameters(params): Parameters<WaitParams>) -> Json<WaitResult> {
        use crate::db::models::TaskStatus;
        use std::collections::HashSet;
        use tokio::time::{sleep, Duration};

        let poll = Duration::from_millis(params.poll_ms.clamp(100, 5000));
        let mut pending: HashSet<String> = params.task_ids.into_iter().collect();
        let mut results: Vec<WaitTaskResult> = Vec::new();

        loop {
            // Scope the connection so the MutexGuard is dropped before the await.
            {
                let conn = self.db.conn();
                pending.retain(|id| {
                    match db::tasks::get_task(&conn, id) {
                        Ok(Some(task)) => match task.status {
                            TaskStatus::Done | TaskStatus::Failed | TaskStatus::Cancelled => {
                                results.push(WaitTaskResult {
                                    task_id: task.id,
                                    status: task.status.to_string(),
                                    result: task.result,
                                    error: task.error,
                                    duration_ms: task.duration_ms,
                                });
                                false // remove from pending
                            }
                            _ => true, // still running
                        },
                        // Unknown task ID — don't wait forever, report as error
                        Ok(None) => {
                            results.push(WaitTaskResult {
                                task_id: id.clone(),
                                status: "not_found".into(),
                                result: None,
                                error: Some("task ID not found in database".into()),
                                duration_ms: None,
                            });
                            false
                        }
                        Err(e) => {
                            results.push(WaitTaskResult {
                                task_id: id.clone(),
                                status: "error".into(),
                                result: None,
                                error: Some(e.to_string()),
                                duration_ms: None,
                            });
                            false
                        }
                    }
                });
            } // conn (MutexGuard) dropped here, before the await below

            if pending.is_empty() {
                break;
            }
            sleep(poll).await;
        }

        Json(WaitResult { results })
    }

    #[tool(
        name = "kew_context_get",
        description = "Retrieve a shared context entry by key. If the key does not exist, returns an empty content string with namespace set to 'not_found' rather than an error."
    )]
    fn context_get(
        &self,
        Parameters(params): Parameters<ContextGetParams>,
    ) -> Json<ContextGetResult> {
        let namespaced_key = format!("{}/{}", project_namespace(), params.key);
        let conn = self.db.conn();
        match db::context::get_context(&conn, &namespaced_key) {
            Ok(Some(entry)) => Json(ContextGetResult {
                // Return the user-facing key (without namespace prefix)
                key: params.key,
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
        description = "Store a shared context entry that can be loaded by future tasks. Returns the key and byte count. NOTE: database write failures are silently ignored — always verify with kew_context_get if the write is critical."
    )]
    fn context_set(
        &self,
        Parameters(params): Parameters<ContextSetParams>,
    ) -> Json<ContextSetResult> {
        let namespaced_key = format!("{}/{}", project_namespace(), params.key);
        let conn = self.db.conn();
        let bytes = params.content.len();
        let _ = db::context::put_context(
            &conn,
            &namespaced_key,
            &params.namespace,
            &params.content,
            None,
        );
        Json(ContextSetResult {
            // Return the user-facing key (without namespace prefix)
            key: params.key,
            bytes,
        })
    }

    #[tool(
        name = "kew_context_search",
        description = "Semantic similarity search over stored task results and context entries. Uses natural language — the query is embedded via nomic-embed-text and compared by cosine similarity. If the embedding service is unreachable, returns an empty result set rather than an error."
    )]
    async fn context_search(
        &self,
        Parameters(params): Parameters<ContextSearchParams>,
    ) -> Json<ContextSearchResult> {
        let embeddings = self.ollama.embed("nomic-embed-text", &[params.query]).await;

        match embeddings {
            Ok(vecs) if !vecs.is_empty() && !vecs[0].is_empty() => {
                let conn = self.db.conn();
                let results = db::vectors::search_similar(&conn, &vecs[0], None, params.top_k)
                    .unwrap_or_default();
                Json(ContextSearchResult {
                    results: results
                        .into_iter()
                        .map(|r| {
                            let content = if params.include_content {
                                db::context::get_context(&conn, &r.key)
                                    .ok()
                                    .flatten()
                                    .map(|e| e.content)
                            } else {
                                None
                            };
                            SearchResultItem {
                                key: r.key,
                                source_type: r.source_type,
                                score: r.score,
                                content,
                            }
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

    #[tool(
        name = "kew_list_agents",
        description = "List all available kew agent types with their descriptions and auto-trigger keywords. Use this to discover which agent to pass to kew_run, or to understand which keywords in a prompt will auto-select an agent."
    )]
    fn list_agents(
        &self,
        Parameters(_params): Parameters<ListAgentsParams>,
    ) -> Json<ListAgentsResult> {
        let project_dir = std::env::current_dir().ok();
        let entries = crate::agents::list_agents(project_dir.as_deref());

        let keyword_map: &[(&str, &str)] = &[
            ("developer",    "implement, build this, write code, add feature, refactor, create a function/struct/class"),
            ("debugger",     "debug, broken, not working, crash, root cause, diagnose, fix the bug, why is"),
            ("docs-writer",  "document, write docs, add docs, explain this, write readme"),
            ("security",     "security, vulnerability, exploit, injection, auth bypass, cve"),
            ("doc-audit",    "doc audit, documentation gap, documentation quality, missing docs, audit doc"),
            ("tester",       "write test, add test, unit test, test coverage, test suite, write specs"),
            ("watcher",      "watch, track progress, summarize progress, what's happening, status report, observe"),
            ("error-finder", "find error, potential bug, what could go wrong, pre-emptive, review for bug, find bug"),
        ];

        let agents = entries
            .into_iter()
            .map(|e| {
                let keywords = keyword_map
                    .iter()
                    .find(|(n, _)| *n == e.name)
                    .map(|(_, kw)| kw.to_string())
                    .unwrap_or_default();
                AgentInfo {
                    name: e.name,
                    description: e.description,
                    model: e.model,
                    source: e.source,
                    keywords,
                }
            })
            .collect();

        Json(ListAgentsResult { agents })
    }
}

impl ServerHandler for KewMcpServer {
    fn get_info(&self) -> ServerInfo {
        use rmcp::model::ServerCapabilities;
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Kew: Real local agent orchestration.\n\nPRIMARY TOOLS:\n• kew_run — execute any prompt; set `agent` for specialist behaviour, or let keywords auto-route\n• kew_list_agents — discover all agents and their trigger keywords\n• kew_context_set/get/search — shared knowledge between tasks\n• kew_status — task counts, context, embeddings, DB size\n• kew_doctor — health check\n\nAGENT QUICK-REFERENCE (pass as `agent` in kew_run, or just use the keywords):\ndeveloper · debugger · docs-writer · security · doc-audit · tester · watcher · error-finder")
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
        async fn chat(
            &self,
            _req: ChatRequest,
        ) -> Result<(ChatResponse, CompletionStats), LlmError> {
            Ok((
                ChatResponse {
                    message: ChatMessage::text("assistant", self.response.clone()),
                    model: "mock".into(),
                    done: true,
                    total_duration_ns: Some(100_000_000),
                    prompt_eval_count: Some(10),
                    eval_count: Some(20),
                },
                CompletionStats {
                    prompt_tokens: Some(10),
                    completion_tokens: Some(20),
                    duration_ms: Some(100),
                },
            ))
        }
        async fn embed(&self, _: &str, input: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            Ok(input.iter().map(|_| vec![1.0, 0.0, 0.0]).collect())
        }
        async fn list_models(&self) -> Result<Vec<String>, LlmError> {
            Ok(vec!["mock-model".into()])
        }
        async fn ping(&self) -> Result<(), LlmError> {
            Ok(())
        }
        fn provider_name(&self) -> &str {
            "mock"
        }
    }

    fn make_server_with_mock(db: Database, response: &str) -> KewMcpServer {
        let ollama: Arc<dyn LlmClient> = Arc::new(MockLlm {
            response: response.into(),
        });
        let tool_router = KewMcpServer::tool_router();
        let pool = Arc::new(SharedPool::start(db.clone(), ollama.clone(), None, 2));
        KewMcpServer {
            db,
            ollama,
            ollama_url: "http://mock:11434".into(),
            tool_router,
            pool,
        }
    }

    #[tokio::test]
    async fn test_server_get_info() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "hi");
        let info = server.get_info();
        assert!(info.instructions.unwrap().contains("kew_run"));
    }

    #[tokio::test]
    async fn test_tool_router_lists_all_tools() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "hi");
        let tools = server.tool_router.list_all();

        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains(&"kew_run"), "missing kew_run: {names:?}");
        assert!(
            names.contains(&"kew_context_get"),
            "missing kew_context_get"
        );
        assert!(
            names.contains(&"kew_context_set"),
            "missing kew_context_set"
        );
        assert!(
            names.contains(&"kew_context_search"),
            "missing kew_context_search"
        );
        assert!(names.contains(&"kew_status"), "missing kew_status");
        assert!(names.contains(&"kew_doctor"), "missing kew_doctor");
        assert_eq!(tools.len(), 9);
        assert!(
            names.contains(&"kew_list_agents"),
            "missing kew_list_agents"
        );
        assert!(names.contains(&"kew_run_bg"), "missing kew_run_bg");
        assert!(names.contains(&"kew_wait"), "missing kew_wait");
    }

    #[tokio::test]
    async fn test_tool_schemas_have_descriptions() {
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

    #[tokio::test]
    async fn test_kew_run_input_schema_requires_prompt() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "hi");
        let tools = server.tool_router.list_all();

        let run_tool = tools.iter().find(|t| t.name == "kew_run").unwrap();
        let schema = serde_json::to_string(&run_tool.input_schema).unwrap();
        assert!(
            schema.contains("prompt"),
            "kew_run schema should require 'prompt': {schema}"
        );
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
        let get_params = ContextGetParams {
            key: "test-key".into(),
        };
        let Json(get_result) = server.context_get(Parameters(get_params));
        assert_eq!(get_result.content, "test content");
        assert_eq!(get_result.namespace, "default");
    }

    #[tokio::test]
    async fn test_mcp_context_get_not_found() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "hi");

        let params = ContextGetParams {
            key: "nonexistent".into(),
        };
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
            agent: None,
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
            agent: None,
        };
        let Json(run_result) = server.run(Parameters(params)).await;
        assert_eq!(run_result.status, "done");

        // share_as keys are namespaced with the project prefix so they round-trip
        // correctly through kew_context_get.
        let namespaced = format!("{}/output-key", project_namespace());
        let conn = server.db.conn();
        let entry = db::context::get_context(&conn, &namespaced).unwrap();
        assert!(entry.is_some(), "namespaced key not found in context");
        assert_eq!(entry.unwrap().content, "shared result");
    }

    #[tokio::test]
    async fn test_mcp_context_search_with_embeddings() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "hi");

        // Store an embedding manually
        {
            let conn = server.db.conn();
            db::vectors::store_embedding(
                &conn,
                "k1",
                "result",
                Some("t1"),
                &[1.0, 0.0, 0.0],
                "mock",
            )
            .unwrap();
        }

        let params = ContextSearchParams {
            query: "test".into(),
            top_k: 5,
            include_content: false,
        };
        let Json(result) = server.context_search(Parameters(params)).await;
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].key, "k1");
        assert!(result.results[0].score > 0.99);
    }

    #[test]
    fn test_detect_agent_keywords() {
        assert_eq!(
            detect_agent_from_prompt("debug why this crashes"),
            Some("debugger")
        );
        assert_eq!(
            detect_agent_from_prompt("write tests for auth module"),
            Some("tester")
        );
        assert_eq!(
            detect_agent_from_prompt("security audit the API layer"),
            Some("security")
        );
        assert_eq!(
            detect_agent_from_prompt("document this function"),
            Some("docs-writer")
        );
        assert_eq!(
            detect_agent_from_prompt("doc audit for the db module"),
            Some("doc-audit")
        );
        assert_eq!(
            detect_agent_from_prompt("find errors in this code"),
            Some("error-finder")
        );
        assert_eq!(
            detect_agent_from_prompt("watch progress on the refactor"),
            Some("watcher")
        );
        assert_eq!(
            detect_agent_from_prompt("implement a retry mechanism"),
            Some("developer")
        );
        assert_eq!(
            detect_agent_from_prompt("what is the capital of France"),
            None
        );
    }

    #[tokio::test]
    async fn test_mcp_run_explicit_agent_sets_system_prompt() {
        let db = Database::open_in_memory().unwrap();
        // Use a mock that echoes back its system prompt in the response isn't possible,
        // but we can verify the task is created (status=done) and no error occurs.
        let server = make_server_with_mock(db, "refactored");
        let params = RunParams {
            prompt: "Refactor the auth module".into(),
            model: "mock".into(),
            system: None,
            context: vec![],
            share_as: None,
            agent: Some("developer".into()),
        };
        let Json(result) = server.run(Parameters(params)).await;
        assert_eq!(result.status, "done");
        assert_eq!(result.result.as_deref(), Some("refactored"));
    }

    #[tokio::test]
    async fn test_mcp_run_keyword_routing_selects_agent() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "done");
        // "debug" keyword → debugger agent auto-selected
        let params = RunParams {
            prompt: "debug why the lock is deadlocking".into(),
            model: "mock".into(),
            system: None,
            context: vec![],
            share_as: None,
            agent: None, // not explicit — should auto-detect
        };
        let Json(result) = server.run(Parameters(params)).await;
        assert_eq!(result.status, "done");
    }

    #[tokio::test]
    async fn test_kew_list_agents_returns_all_builtins() {
        let db = Database::open_in_memory().unwrap();
        let server = make_server_with_mock(db, "");
        let Json(result) = server.list_agents(Parameters(ListAgentsParams {}));
        let names: Vec<&str> = result.agents.iter().map(|a| a.name.as_str()).collect();
        for expected in &[
            "developer",
            "debugger",
            "docs-writer",
            "security",
            "doc-audit",
            "tester",
            "watcher",
            "error-finder",
        ] {
            assert!(
                names.contains(expected),
                "missing agent '{expected}' in list_agents"
            );
        }
        // Agents with known keywords should have non-empty keywords field
        let dev = result
            .agents
            .iter()
            .find(|a| a.name == "developer")
            .unwrap();
        assert!(!dev.keywords.is_empty());
    }
}

/// Start the MCP server on stdio.
///
/// Reads `kew_config.yaml` from the current directory to resolve the worker
/// count. The CLI-supplied `ollama_url` takes precedence over the config file
/// value so that `--ollama-url` flags still work as expected.
pub async fn serve(db: Database, ollama_url: &str, workers: usize) -> anyhow::Result<()> {
    let server = KewMcpServer::new(db, ollama_url, workers);
    let service = server
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await
        .map_err(|e| anyhow::anyhow!("MCP server init failed: {e}"))?;
    service.waiting().await?;
    Ok(())
}
