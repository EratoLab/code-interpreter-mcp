use std::{
    borrow::Cow,
    collections::HashSet,
    io::{self, Write},
    net::SocketAddr,
    path::{Component, Path},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Router,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use rmcp::{
    Json, ServerHandler,
    handler::server::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{ProgressNotificationParam, ProtocolVersion},
    schemars,
    service::{RequestContext, RoleServer},
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::{Deserialize, Serialize};

mod runtime;
mod tokio_workers;
use code_interpreter_mcp::config::{AppConfig, CodeSnippetsConfig};
use code_interpreter_mcp::input_limits::{BYTES_PER_KB, InputLimits, MAX_CODE_BYTES, MAX_FILES};
use runtime::PyodideRuntime;

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 120_000;
const RUN_PYTHON_CODE_DESCRIPTION: &str = "Run Python code in a fresh Pyodide WebAssembly environment using a bounded worker queue. Queued calls may send MCP progress updates when the client supplies a progress token. Supports top-level await and Base64-encoded input files mounted below /workspace. Returns captured stdout, stderr, the final expression, Base64-encoded files created by the script, and a structured error when execution fails.";

fn run_python_code_description(config: &AppConfig) -> String {
    let mut description = RUN_PYTHON_CODE_DESCRIPTION.to_owned();
    if config.runtime_package_installation {
        description.push_str(" Supports micropip.install() for compatible Python wheels.");
    }
    let packages: Vec<_> = config
        .preinstall_packages
        .iter()
        .filter_map(|requirement| package_name(requirement))
        .collect();
    if !packages.is_empty() {
        description.push_str(" Pre-installed libraries: ");
        description.push_str(&packages.join(", "));
        description.push('.');
    }
    description
}

fn package_name(requirement: &str) -> Option<String> {
    let requirement = requirement.trim();
    if let Ok(url) = reqwest::Url::parse(requirement) {
        // Wheel filenames start with the distribution name, followed by the
        // version and compatibility tags. Query strings and fragments are not
        // part of the filename.
        let filename = url.path().rsplit('/').next()?;
        let (name, _) = filename.strip_suffix(".whl")?.split_once('-')?;
        return (!name.is_empty()).then(|| name.to_owned());
    }
    // Named requirements start with the distribution name, before any extras,
    // version constraints, environment markers, or direct-reference URL.
    let name = requirement
        .split(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '.' | '_' | '-'))
        .next()?;
    (!name.is_empty()).then(|| name.to_owned())
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RunPythonCodeRequest {
    /// Python source code. Top-level await is supported.
    code: String,

    /// Files to create below /workspace before running the code.
    #[serde(default)]
    files: Vec<InputFile>,

    /// Execution timeout in milliseconds. Defaults to 30000; maximum 120000.
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct InputFile {
    /// Relative path below /workspace. Absolute paths and parent traversal are rejected.
    path: String,

    /// File contents encoded as canonical RFC 4648 Base64.
    #[schemars(extend(
        "contentEncoding" = "base64",
        "chat.erato/file_content_field" = true
    ))]
    content_base64: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct RunPythonCodeOutput {
    success: bool,
    stdout: String,
    stderr: String,
    result: Option<String>,
    /// Omitted when the script creates no files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    files: Vec<OutputFile>,
    error: Option<ExecutionError>,
    timed_out: bool,
    output_truncated: bool,
}

impl RunPythonCodeOutput {
    fn validation(message: impl Into<String>) -> Self {
        Self::failure("validation", message, None, false)
    }

    pub(crate) fn runtime(message: impl Into<String>) -> Self {
        Self::failure("runtime", message, None, false)
    }

    pub(crate) fn timed_out(timeout_ms: u64) -> Self {
        Self::failure(
            "timeout",
            format!("Python execution exceeded the {timeout_ms} ms timeout"),
            None,
            true,
        )
    }

    fn overloaded() -> Self {
        Self::failure(
            "overloaded",
            "The code execution queue is full; retry after a worker becomes available",
            None,
            false,
        )
    }

    fn failure(
        kind: impl Into<String>,
        message: impl Into<String>,
        traceback: Option<String>,
        timed_out: bool,
    ) -> Self {
        Self {
            success: false,
            stdout: String::new(),
            stderr: String::new(),
            result: None,
            files: Vec::new(),
            error: Some(ExecutionError {
                kind: kind.into(),
                message: message.into(),
                traceback,
            }),
            timed_out,
            output_truncated: false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct OutputFile {
    /// Path relative to /workspace.
    #[schemars(extend("chat.erato/file_name_field" = true))]
    path: String,

    /// File contents encoded as canonical RFC 4648 Base64.
    #[schemars(extend(
        "contentEncoding" = "base64",
        "chat.erato/file_content_field" = true
    ))]
    content_base64: String,

    /// MIME type inferred from the file extension.
    mime_type: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct ExecutionError {
    kind: String,
    message: String,
    traceback: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RuntimeRequest {
    pub(crate) code: String,
    pub(crate) files: Vec<ValidatedFile>,
    pub(crate) timeout_ms: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct ValidatedFile {
    pub(crate) path: String,
    pub(crate) content_base64: String,
}

#[derive(Clone)]
struct CodeInterpreterMcpServer {
    runtime: PyodideRuntime,
    code_snippets: CodeSnippetsConfig,
    search_min_hits: usize,
    input_limits: InputLimits,
    tool_router: ToolRouter<Self>,
}

#[tool_router(router = tool_router)]
impl CodeInterpreterMcpServer {
    async fn new(config: &AppConfig) -> Result<Self> {
        let input_limits = config.input_limits.validate()?;
        let code_snippets = CodeSnippetsConfig {
            enabled: config.code_snippets.enabled,
            snippets: config
                .code_snippets
                .snippets
                .iter()
                .filter(|(id, _)| snippet_is_included(id, &config.code_snippets))
                .map(|(id, snippet)| (id.clone(), snippet.clone()))
                .collect(),
            include_snippet_ids: Vec::new(),
            include_snippet_tags: Vec::new(),
            exclude_snippet_ids: Vec::new(),
        };
        Ok(Self {
            runtime: PyodideRuntime::new(config).await?,
            code_snippets,
            search_min_hits: config.search.min_hits,
            input_limits,
            tool_router: Self::configured_tool_router(config),
        })
    }

    fn configured_tool_router(config: &AppConfig) -> ToolRouter<Self> {
        let mut tool_router = Self::tool_router();
        tool_router
            .map
            .get_mut("run_python_code")
            .expect("run_python_code route is registered")
            .attr
            .description = Some(run_python_code_description(config).into());
        if !config.code_snippets.enabled {
            tool_router.disable_route("list_code_snippets");
            tool_router.disable_route("get_code_snippet");
        }
        tool_router
    }

    #[tool(description = RUN_PYTHON_CODE_DESCRIPTION)]
    async fn run_python_code(
        &self,
        Parameters(request): Parameters<RunPythonCodeRequest>,
        context: RequestContext<RoleServer>,
    ) -> Json<RunPythonCodeOutput> {
        let request = match validate_request(request, self.input_limits) {
            Ok(request) => request,
            Err(error) => {
                return Json(RunPythonCodeOutput::validation(error.to_string()));
            }
        };

        let cancellation = Arc::new(AtomicBool::new(false));
        let (job_id, mut response) = match self.runtime.submit(request, cancellation.clone()) {
            Ok(submitted) => submitted,
            Err(_) if !self.runtime.health().is_ready() => {
                return Json(RunPythonCodeOutput::runtime(
                    "Worker pool is not accepting executions",
                ));
            }
            Err(_) => return Json(RunPythonCodeOutput::overloaded()),
        };

        let mut progress_token = context.meta.get_progress_token();
        let peer = context.peer;
        let request_cancellation = context.ct;
        let mut progress_ticks = 0u64;
        let progress_start = tokio::time::Instant::now() + Duration::from_secs(1);
        let mut progress_interval =
            tokio::time::interval_at(progress_start, Duration::from_secs(5));
        loop {
            tokio::select! {
                result = &mut response => {
                    return Json(result.unwrap_or_else(|_| RunPythonCodeOutput::runtime("Worker ended before returning a result")));
                }
                _ = request_cancellation.cancelled() => {
                    cancellation.store(true, Ordering::Release);
                    self.runtime.cancel_queued(job_id);
                    return Json(RunPythonCodeOutput::runtime("Request cancelled while waiting for a worker"));
                }
                _ = progress_interval.tick(), if progress_token.is_some() => {
                    let Some(position) = self.runtime.queue_position(job_id) else {
                        progress_token = None;
                        continue;
                    };
                    progress_ticks += 1;
                    let notification = ProgressNotificationParam::new(
                        progress_token.clone().expect("guarded by progress token option"),
                        progress_ticks as f64,
                    )
                    .with_message(format!("Waiting for worker slot. Position {position} in the queue."));
                    if let Err(error) = peer.notify_progress(notification).await {
                        eprintln!("Could not send MCP queue progress notification: {error}");
                    }
                }
            }
        }
    }

    #[tool(
        description = "List configured code snippets by ID, title, description, tags, and required Python packages. Search ranks partial query matches in titles and descriptions without returning the code. Depending on search.min_hits, weak or unrelated fallback results may be included; check their descriptions before use."
    )]
    fn list_code_snippets(
        &self,
        Parameters(request): Parameters<ListCodeSnippetsRequest>,
    ) -> Json<ListCodeSnippetsOutput> {
        let snippets = code_interpreter_mcp::snippet_search::search_snippets(
            &self.code_snippets.snippets,
            request.search.as_deref(),
            self.search_min_hits,
        )
        .into_iter()
        .map(|(id, snippet)| CodeSnippetSummary {
            id: id.clone(),
            title: snippet.title.clone(),
            description: snippet.description.clone(),
            required_python_packages: snippet.required_python_packages.clone(),
            tags: snippet.tags.clone(),
        })
        .collect();
        Json(ListCodeSnippetsOutput { snippets })
    }

    #[tool(
        description = "Return the full metadata and Python source for a configured code snippet by its ID."
    )]
    fn get_code_snippet(
        &self,
        Parameters(request): Parameters<GetCodeSnippetRequest>,
    ) -> Json<GetCodeSnippetOutput> {
        let snippet =
            self.code_snippets
                .snippets
                .get(&request.id)
                .map(|snippet| CodeSnippetDetails {
                    id: request.id,
                    title: snippet.title.clone(),
                    description: snippet.description.clone(),
                    required_python_packages: snippet.required_python_packages.clone(),
                    tags: snippet.tags.clone(),
                    code_snippet: snippet.code_snippet.clone(),
                });
        Json(GetCodeSnippetOutput { snippet })
    }
}

#[tool_handler(router = self.tool_router,
    name = "code-interpreter-mcp",
    version = "0.1.0",
    instructions = "Use run_python_code to execute Python in a fresh Pyodide WebAssembly sandbox."
)]
impl ServerHandler for CodeInterpreterMcpServer {
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        // Older protocol versions use Streamable HTTP's initialize lifecycle.
        // The 2026-07-28 protocol uses stateless request metadata instead; rmcp
        // routes it through the stateless HTTP path while keeping legacy clients
        // on sessions.
        Cow::Borrowed(&[
            ProtocolVersion::V_2025_03_26,
            ProtocolVersion::V_2025_06_18,
            ProtocolVersion::V_2025_11_25,
            ProtocolVersion::V_2026_07_28,
        ])
    }

    fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::DiscoverResult, rmcp::ErrorData>> + Send + '_
    {
        std::future::ready(Ok(rmcp::model::DiscoverResult::from_server_info(
            self.supported_protocol_versions().into_owned(),
            self.get_info(),
        )
        .with_ttl_ms(0)
        .with_cache_scope(rmcp::model::CacheScope::Public)))
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListCodeSnippetsRequest {
    /// Optional case-insensitive keywords over titles and descriptions. Partial matches are ranked by relevance; not every word needs to match.
    search: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct ListCodeSnippetsOutput {
    snippets: Vec<CodeSnippetSummary>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct CodeSnippetSummary {
    id: String,
    title: String,
    description: String,
    required_python_packages: Vec<String>,
    tags: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetCodeSnippetRequest {
    /// Configured snippet ID.
    id: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GetCodeSnippetOutput {
    /// Null when no configured snippet has this ID.
    snippet: Option<CodeSnippetDetails>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct CodeSnippetDetails {
    id: String,
    title: String,
    description: String,
    required_python_packages: Vec<String>,
    tags: Vec<String>,
    code_snippet: String,
}

fn snippet_is_included(id: &str, config: &CodeSnippetsConfig) -> bool {
    if config
        .exclude_snippet_ids
        .iter()
        .any(|excluded| excluded == id)
    {
        return false;
    }
    let has_include_filter =
        !config.include_snippet_ids.is_empty() || !config.include_snippet_tags.is_empty();
    !has_include_filter
        || config
            .include_snippet_ids
            .iter()
            .any(|included| included == id)
        || config.snippets.get(id).is_some_and(|snippet| {
            snippet
                .tags
                .iter()
                .any(|tag| config.include_snippet_tags.contains(tag))
        })
}

fn validate_request(request: RunPythonCodeRequest, limits: InputLimits) -> Result<RuntimeRequest> {
    if request.code.len() > MAX_CODE_BYTES {
        bail!("code exceeds the {MAX_CODE_BYTES} byte limit");
    }
    if request.files.len() > MAX_FILES {
        bail!("at most {MAX_FILES} files may be supplied");
    }

    let timeout_ms = request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    if timeout_ms == 0 || timeout_ms > MAX_TIMEOUT_MS {
        bail!("timeout_ms must be between 1 and {MAX_TIMEOUT_MS}");
    }

    let mut seen = HashSet::new();
    let mut total_bytes = 0usize;
    let mut files = Vec::with_capacity(request.files.len());
    for file in request.files {
        let path = validate_relative_path(&file.path)?;
        if !seen.insert(path.clone()) {
            bail!("duplicate file path: {path}");
        }

        if file.content_base64.len() > limits.max_encoded_file_bytes() {
            bail!(
                "input file {path} exceeds the per-file limit: {} encoded bytes exceed the {} byte Base64 allowance (input_limits.max_file_kb = {} KB)",
                file.content_base64.len(),
                limits.max_encoded_file_bytes(),
                limits.max_file_bytes() / BYTES_PER_KB
            );
        }
        let decoded = BASE64
            .decode(file.content_base64.as_bytes())
            .with_context(|| format!("file {path} is not valid canonical Base64"))?;
        if decoded.len() > limits.max_file_bytes() {
            bail!(
                "input file {path} exceeds the per-file limit: {} decoded bytes > per-file allowance of {} decoded bytes (input_limits.max_file_kb = {} KB)",
                decoded.len(),
                limits.max_file_bytes(),
                limits.max_file_bytes() / BYTES_PER_KB
            );
        }
        total_bytes = total_bytes
            .checked_add(decoded.len())
            .ok_or_else(|| anyhow!("total file size overflowed"))?;
        if total_bytes > limits.max_total_file_bytes() {
            bail!(
                "input files exceed the aggregate limit at {path}: {total_bytes} decoded bytes > aggregate allowance of {} decoded bytes (input_limits.max_total_file_kb = {} KB)",
                limits.max_total_file_bytes(),
                limits.max_total_file_bytes() / BYTES_PER_KB
            );
        }

        files.push(ValidatedFile {
            path,
            content_base64: BASE64.encode(decoded),
        });
    }

    Ok(RuntimeRequest {
        code: request.code,
        files,
        timeout_ms,
    })
}

fn validate_relative_path(value: &str) -> Result<String> {
    if value.is_empty() || value.contains('\0') || value.contains('\\') {
        bail!("file paths must be non-empty POSIX relative paths");
    }

    let path = Path::new(value);
    if path.is_absolute() {
        bail!("absolute file path is not allowed: {value}");
    }

    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            _ => bail!("file path may not contain '.' or '..': {value}"),
        }
    }
    if parts.is_empty() {
        bail!("file path must name a file");
    }
    Ok(parts.join("/"))
}

fn http_server_config(config: &AppConfig, limits: InputLimits) -> StreamableHttpServerConfig {
    let http_config = StreamableHttpServerConfig::default()
        .with_max_request_body_bytes(limits.max_request_body_bytes());
    if config.skip_host_header_verification {
        http_config.disable_allowed_hosts()
    } else if config.http_allowed_hosts.is_empty() {
        http_config
    } else {
        http_config.with_allowed_hosts(config.http_allowed_hosts.clone())
    }
}

fn http_router(config: &AppConfig, server: CodeInterpreterMcpServer) -> Router {
    let health = server.runtime.health();
    let ready_health = health.clone();
    let limits = server.input_limits;
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        http_server_config(config, limits),
    );
    Router::new()
        .route("/livez", get(move || async move { probe_response(health.is_live()) }))
        .route("/readyz", get(move || async move { probe_response(ready_health.is_ready()) }))
        .nest_service("/mcp", service).layer(middleware::map_response(
        move |response: Response| async move {
            if response.status() != StatusCode::PAYLOAD_TOO_LARGE {
                return response;
            }
            let message = format!(
                "HTTP payload too large: request body exceeds {} bytes. This ceiling is derived from input_limits.max_total_file_kb = {} KB, Base64 padding for up to {MAX_FILES} files, and JSON/RPC headroom. Increase the aggregate allowance for larger requests; input_limits.max_file_kb = {} KB still applies to each file.",
                limits.max_request_body_bytes(), limits.max_total_file_bytes() / BYTES_PER_KB, limits.max_file_bytes() / BYTES_PER_KB
            );
            eprintln!("{message}");
            (StatusCode::PAYLOAD_TOO_LARGE, message).into_response()
        },
    ))
}

fn probe_response(healthy: bool) -> (StatusCode, &'static str) {
    if healthy {
        (StatusCode::OK, "ok\n")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "unavailable\n")
    }
}

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("failed to install SIGTERM handler")?;
        tokio::select! {
            _ = terminate.recv() => {},
            result = tokio::signal::ctrl_c() => result.context("failed to listen for Ctrl-C")?,
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .context("failed to listen for Ctrl-C")?;
    Ok(())
}

async fn serve_until_shutdown(
    listener: tokio::net::TcpListener,
    router: Router,
    runtime: PyodideRuntime,
    signal: impl Future<Output = Result<()>>,
    drain_timeout: Duration,
) -> Result<()> {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let serving = axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = stop_rx.await;
        })
        .into_future();
    tokio::pin!(serving);
    tokio::select! {
        result = &mut serving => return result.context("MCP Streamable HTTP server failed"),
        result = signal => result?,
    }
    let deadline = tokio::time::Instant::now() + drain_timeout;
    runtime.shutdown();
    eprintln!(
        "Shutdown started: queued work cancelled; draining active work for up to {drain_timeout:?}"
    );
    // Keep serving probes while active workers drain. No new executions are accepted.
    let drain = async {
        while runtime.health().live_workers() > 0 {
            tokio::select! {
                result = &mut serving => return result.context("MCP Streamable HTTP server failed"),
                _ = tokio::time::sleep(Duration::from_millis(25)) => {},
            }
        }
        let _ = stop_tx.send(());
        (&mut serving)
            .await
            .context("MCP Streamable HTTP server failed")
    };
    match tokio::time::timeout_at(deadline, drain).await {
        Ok(result) => result,
        Err(_) => {
            eprintln!("Shutdown deadline reached; cutting off remaining work and HTTP streams");
            Ok(())
        }
    }
}

fn main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(tokio_workers::configured_worker_threads())
        .enable_all()
        .build()
        .context("failed to build Tokio runtime")?;
    let result = runtime.block_on(async_main());
    // Do not let persistent HTTP streams or blocking tasks extend the deadline.
    runtime.shutdown_timeout(Duration::ZERO);
    result
}

async fn async_main() -> Result<()> {
    let config = AppConfig::load()?;
    let bind_address: SocketAddr = config
        .bind_addr
        .parse()
        .with_context(|| format!("invalid bind_addr: {}", config.bind_addr))?;

    let server = CodeInterpreterMcpServer::new(&config).await?;
    eprintln!(
        "Input size limits: {} decoded bytes per file, {} decoded bytes total; HTTP request body ceiling: {} bytes",
        server.input_limits.max_file_bytes(),
        server.input_limits.max_total_file_bytes(),
        server.input_limits.max_request_body_bytes(),
    );
    let worker_runtime = server.runtime.clone();
    let router = http_router(&config, server);
    let listener = tokio::net::TcpListener::bind(bind_address)
        .await
        .with_context(|| format!("failed to bind MCP HTTP server to {bind_address}"))?;
    let listening_address = listener
        .local_addr()
        .context("failed to read MCP HTTP listening address")?;

    println!(
        "code-interpreter-mcp listening on port {} at http://{listening_address}/mcp",
        listening_address.port()
    );
    io::stdout()
        .flush()
        .context("failed to flush MCP HTTP startup announcement")?;

    serve_until_shutdown(
        listener,
        router,
        worker_runtime,
        shutdown_signal(),
        SHUTDOWN_TIMEOUT,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn test_server(runtime: PyodideRuntime) -> (AppConfig, CodeInterpreterMcpServer) {
        let config: AppConfig = toml::from_str("").unwrap();
        let server = CodeInterpreterMcpServer {
            runtime,
            code_snippets: config.code_snippets.clone(),
            search_min_hits: config.search.min_hits,
            input_limits: config.input_limits.validate().unwrap(),
            tool_router: CodeInterpreterMcpServer::configured_tool_router(&config),
        };
        (config, server)
    }

    async fn check_probe(client: &reqwest::Client, base: &str, path: &str, status: StatusCode) {
        let response = client
            .get(format!("{base}/{path}"))
            .header("Host", "10.42.0.17:8000")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(
            response.text().await.unwrap(),
            if status == StatusCode::OK {
                "ok\n"
            } else {
                "unavailable\n"
            }
        );
    }

    #[tokio::test]
    async fn probes_accept_pod_hosts_and_report_saturation_and_worker_loss() {
        let (runtime, worker) = PyodideRuntime::test_pool();
        let (config, server) = test_server(runtime.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, http_router(&config, server))
                .await
                .unwrap()
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let request = || RuntimeRequest {
            code: "pass".into(),
            files: vec![],
            timeout_ms: 1000,
        };
        let _queued = runtime
            .submit(request(), Arc::new(AtomicBool::new(false)))
            .ok()
            .unwrap();
        assert!(
            runtime
                .submit(request(), Arc::new(AtomicBool::new(false)))
                .is_err()
        );
        for path in ["livez", "readyz"] {
            check_probe(&client, &base, path, StatusCode::OK).await;
        }
        let response = client
            .post(format!("{base}/mcp"))
            .header("Host", "10.42.0.17:8000")
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body(r#"{"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{}}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        drop(worker);
        for path in ["livez", "readyz"] {
            check_probe(&client, &base, path, StatusCode::SERVICE_UNAVAILABLE).await;
        }
        task.abort();
    }

    #[tokio::test]
    async fn shutdown_keeps_liveness_during_drain_and_bounds_stuck_workers() {
        let (runtime, worker) = PyodideRuntime::test_pool();
        let (config, server) = test_server(runtime.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel();
        let serving_runtime = runtime.clone();
        let task = tokio::spawn(serve_until_shutdown(
            listener,
            http_router(&config, server),
            serving_runtime,
            async {
                signal_rx.await.unwrap();
                Ok(())
            },
            Duration::from_millis(300),
        ));
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        check_probe(&client, &base, "readyz", StatusCode::OK).await;
        signal_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while runtime.health().is_ready() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        check_probe(&client, &base, "readyz", StatusCode::SERVICE_UNAVAILABLE).await;
        check_probe(&client, &base, "livez", StatusCode::OK).await;
        assert!(
            runtime
                .submit(
                    RuntimeRequest {
                        code: "pass".into(),
                        files: vec![],
                        timeout_ms: 1000
                    },
                    Arc::new(AtomicBool::new(false))
                )
                .is_err()
        );
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        // The worker is deliberately still alive when the server deadline expires.
        assert_eq!(runtime.health().live_workers(), 1);
        drop(worker);
    }

    #[tokio::test]
    async fn shutdown_deadline_includes_persistent_mcp_streams() {
        let (runtime, worker) = PyodideRuntime::test_pool();
        let (config, server) = test_server(runtime.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(serve_until_shutdown(
            listener,
            http_router(&config, server),
            runtime.clone(),
            async {
                signal_rx.await.unwrap();
                Ok(())
            },
            Duration::from_millis(300),
        ));
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let initialized = client.post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"shutdown-test","version":"1"}}}"#)
            .send().await.unwrap();
        assert_eq!(initialized.status(), StatusCode::OK);
        let session = initialized.headers()["mcp-session-id"].clone();
        let _ = initialized.bytes().await.unwrap();
        let notification = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("Mcp-Session-Id", session.clone())
            .header("MCP-Protocol-Version", "2025-11-25")
            .body(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .send()
            .await
            .unwrap();
        assert!(notification.status().is_success());
        let stream = client
            .get(&url)
            .header("Accept", "text/event-stream")
            .header("Mcp-Session-Id", session)
            .header("MCP-Protocol-Version", "2025-11-25")
            .send()
            .await
            .unwrap();
        assert_eq!(stream.status(), StatusCode::OK);
        signal_tx.send(()).unwrap();
        while runtime.health().is_ready() {
            tokio::task::yield_now().await;
        }
        drop(worker);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        // Keep the stream open until after the server's shutdown future returns.
        drop(stream);
    }

    #[tokio::test]
    async fn snippet_search_respects_visibility_and_configured_floor() {
        let mut config: AppConfig = toml::from_str(
            r#"
            [workers]
            prewarmed_workers = 0
            [search]
            min_hits = 100
            [code_snippets]
            include_snippet_tags = ["visible"]
            exclude_snippet_ids = ["excluded"]
            [code_snippets.snippets.allowed]
            title = "Text document conversion"
            description = "Converts DOCX documents"
            tags = ["visible"]
            code_snippet = "pass"
            [code_snippets.snippets.excluded]
            title = "XLSX risk register"
            description = "Excluded even though tagged visible"
            tags = ["visible"]
            code_snippet = "pass"
            [code_snippets.snippets.hidden]
            title = "XLSX risk register"
            description = "Not included"
            code_snippet = "pass"
            "#,
        )
        .unwrap();
        for (min_hits, expected_count) in [(100, 1), (0, 0)] {
            config.search.min_hits = min_hits;
            let server = CodeInterpreterMcpServer::new(&config).await.unwrap();
            let result = server
                .list_code_snippets(Parameters(ListCodeSnippetsRequest {
                    search: Some("XLSX risk register".to_owned()),
                }))
                .0;
            assert_eq!(result.snippets.len(), expected_count);
            assert!(
                result
                    .snippets
                    .iter()
                    .all(|snippet| snippet.id == "allowed")
            );
            let result = server
                .list_code_snippets(Parameters(ListCodeSnippetsRequest {
                    search: Some("DOCX create document".to_owned()),
                }))
                .0;
            assert_eq!(result.snippets.len(), 1);
            assert_eq!(result.snippets[0].id, "allowed");
        }
    }

    #[test]
    fn advertised_description_reflects_configuration() {
        for installation_enabled in [false, true] {
            for packages in [
                vec![],
                vec![
                    "numpy>=2,<3",
                    "snowballstemmer==3.0.1",
                    "pandas[performance]",
                ],
            ] {
                let mut config: AppConfig = toml::from_str("").unwrap();
                config.runtime_package_installation = installation_enabled;
                config.preinstall_packages = packages.iter().map(|s| (*s).to_owned()).collect();
                // tools/list uses this router's list_all(), including its
                // configured metadata rather than the static tool annotation.
                let tool = CodeInterpreterMcpServer::configured_tool_router(&config)
                    .list_all()
                    .into_iter()
                    .find(|tool| tool.name == "run_python_code")
                    .unwrap();
                let description = tool.description.unwrap();
                assert!(description.contains("Pyodide WebAssembly"));
                assert_eq!(
                    description.contains("micropip.install()"),
                    installation_enabled
                );
                if packages.is_empty() {
                    assert!(!description.contains("Pre-installed libraries:"));
                    assert!(!description.contains("pandas"));
                    assert!(!description.contains("other Pyodide packages"));
                } else {
                    assert!(
                        description
                            .ends_with("Pre-installed libraries: numpy, snowballstemmer, pandas.")
                    );
                    assert!(!description.contains("3.0.1"));
                    assert!(!description.contains(">=2"));
                    assert!(!description.contains("performance"));
                }
            }
        }
    }

    #[test]
    fn extracts_names_from_package_requirements() {
        for (requirement, expected) in [
            ("pandas", "pandas"),
            (" snowballstemmer==3.0.1 ", "snowballstemmer"),
            ("numpy>=2,<3", "numpy"),
            ("numpy~=2.0", "numpy"),
            ("numpy!=2.0", "numpy"),
            ("numpy===2.0", "numpy"),
            ("numpy<=2.0", "numpy"),
            ("numpy>1.0", "numpy"),
            ("numpy (>=2,<3)", "numpy"),
            ("pkg[extra,other]>=1; python_version >= '3.10'", "pkg"),
            ("pkg; python_version >= '3.10'", "pkg"),
            ("my-package_name.dist==1.0", "my-package_name.dist"),
            ("pkg @ https://example.com/pkg-1.0-py3-none-any.whl", "pkg"),
            (
                "https://example.com/snowballstemmer-3.0.1-py3-none-any.whl",
                "snowballstemmer",
            ),
            (
                "https://example.com/my_package-1.0-1-py3-none-any.whl?token=abc#sha256=123",
                "my_package",
            ),
            ("emfs:/wheels/my_package-1.0-py3-none-any.whl", "my_package"),
        ] {
            assert_eq!(
                package_name(requirement).as_deref(),
                Some(expected),
                "{requirement}"
            );
        }
        assert_eq!(package_name(""), None);
        assert_eq!(package_name("https://example.com/not-a-wheel"), None);
    }

    #[tokio::test]
    async fn enforces_configured_http_hosts() {
        for (settings, cases) in [
            (
                "",
                vec![
                    ("localhost:8000", true),
                    ("127.0.0.1", true),
                    ("[::1]:8000", true),
                    ("mcp.example.com", false),
                ],
            ),
            (
                "http_allowed_hosts = []\nallowed_hosts = ['packages.example.com']",
                vec![("localhost", true), ("packages.example.com", false)],
            ),
            (
                "http_allowed_hosts = ['mcp.example.com', 'proxy.example.com:8443']",
                vec![
                    ("mcp.example.com:8000", true),
                    ("proxy.example.com:8443", true),
                    ("proxy.example.com:8000", false),
                    ("localhost", false),
                    ("evil.example.com", false),
                ],
            ),
            (
                "http_allowed_hosts = ['mcp.example.com']\nskip_host_header_verification = true",
                vec![("unlisted.example.com:1234", true)],
            ),
        ] {
            let mut config: AppConfig = toml::from_str(settings).unwrap();
            config.workers.max_workers = 1;
            config.workers.queue_capacity = 1;
            config.workers.prewarmed_workers = 0;
            let server = CodeInterpreterMcpServer::new(&config).await.unwrap();
            let router = http_router(&config, server);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/mcp", listener.local_addr().unwrap());
            let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            for (host, allowed) in cases {
                let response = client
                    .post(&url)
                    .header("Host", host)
                    .header("Content-Type", "application/json")
                    .header("Accept", "application/json, text/event-stream")
                    .body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"host-test","version":"1"}}}"#)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    if allowed {
                        reqwest::StatusCode::OK
                    } else {
                        reqwest::StatusCode::FORBIDDEN
                    },
                    "Host {host} with settings {settings:?}",
                );
            }
            task.abort();
        }
    }

    fn request(path: &str, data: &[u8]) -> RunPythonCodeRequest {
        RunPythonCodeRequest {
            code: "pass".to_owned(),
            files: vec![InputFile {
                path: path.to_owned(),
                content_base64: BASE64.encode(data),
            }],
            timeout_ms: None,
        }
    }

    fn default_limits() -> InputLimits {
        code_interpreter_mcp::input_limits::InputLimitsConfig::default()
            .validate()
            .unwrap()
    }

    fn limits(file: usize, total: usize) -> InputLimits {
        code_interpreter_mcp::input_limits::InputLimitsConfig {
            max_file_kb: file,
            max_total_file_kb: total,
        }
        .validate()
        .unwrap()
    }

    fn multi_file_request(sizes: &[usize]) -> RunPythonCodeRequest {
        RunPythonCodeRequest {
            code: "pass".to_owned(),
            files: sizes
                .iter()
                .enumerate()
                .map(|(index, size)| InputFile {
                    path: format!("file-{index}.bin"),
                    content_base64: BASE64.encode(vec![0; *size]),
                })
                .collect(),
            timeout_ms: None,
        }
    }

    fn rpc_request(input: &RunPythonCodeRequest) -> Value {
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "run_python_code", "arguments": {
                "code": input.code, "timeout_ms": input.timeout_ms,
                "files": input.files.iter().map(|file| serde_json::json!({
                    "path": file.path, "content_base64": file.content_base64
                })).collect::<Vec<_>>()
            }}
        })
    }

    #[test]
    fn configured_file_and_aggregate_boundaries() {
        // One or two padding bytes can mask a decoded overrun if only the
        // encoded length is checked. Exercise every remainder modulo three.
        for max_file_kb in [1, 2, 3, 4, 5, 6] {
            let policy = limits(max_file_kb, max_file_kb * 2);
            let max_file = max_file_kb * BYTES_PER_KB;
            assert!(validate_request(multi_file_request(&[max_file, max_file]), policy).is_ok());
            let error = validate_request(multi_file_request(&[max_file + 1]), policy)
                .unwrap_err()
                .to_string();
            assert!(error.contains("per-file limit"), "{error}");
            assert!(error.contains("input_limits.max_file_kb"), "{error}");
        }
        let error = validate_request(multi_file_request(&[4096, 2049]), limits(4, 6))
            .unwrap_err()
            .to_string();
        assert!(error.contains("aggregate limit"), "{error}");
        assert!(error.contains("6145 decoded bytes"), "{error}");
        assert!(
            error.contains("input_limits.max_total_file_kb = 6 KB"),
            "{error}"
        );
        assert!(validate_request(multi_file_request(&[0, 0]), limits(1, 1)).is_ok());
        assert!(
            validate_request(multi_file_request(&[1; MAX_FILES]), limits(1, MAX_FILES)).is_ok()
        );
        assert!(
            validate_request(multi_file_request(&[0; MAX_FILES + 1]), default_limits()).is_err()
        );
    }

    #[test]
    fn permits_incident_file_and_multiple_large_files_with_json_overhead() {
        let policy = limits(50 * 1024, 100 * 1024);
        let input = multi_file_request(&[41_943_090, 40 * 1024 * 1024]);
        assert!(
            serde_json::to_vec(&rpc_request(&input)).unwrap().len()
                < policy.max_request_body_bytes()
        );
        assert!(validate_request(input, policy).is_ok());

        let policy = limits(1, MAX_FILES);
        let mut input = multi_file_request(&[1; MAX_FILES]);
        input.code = "\0".repeat(MAX_CODE_BYTES); // Worst-case JSON escaping of source.
        for file in &mut input.files {
            file.path.push_str(&"\u{1}".repeat(4096));
        }
        assert!(
            serde_json::to_vec(&rpc_request(&input)).unwrap().len()
                <= policy.max_request_body_bytes()
        );
        assert!(validate_request(input, policy).is_ok());
    }

    #[tokio::test]
    async fn http_size_policy_admits_large_payloads_and_reports_each_rejection() {
        let mut config: AppConfig = toml::from_str("").unwrap();
        config.workers.prewarmed_workers = 0;
        config.input_limits.max_file_kb = 50 * 1024;
        config.input_limits.max_total_file_kb = 60 * 1024;
        let policy = config.input_limits.validate().unwrap();
        let server = CodeInterpreterMcpServer::new(&config).await.unwrap();
        let router = http_router(&config, server);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        let post = || {
            client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
        };
        let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"size-test","version":"1"}}}"#;
        let response = post().body(initialize).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let session = response.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .to_owned();
        // Finish initialize before using tools/call.
        let _ = response.text().await.unwrap();
        let response = post()
            .header("Mcp-Session-Id", &session)
            .header("MCP-Protocol-Version", "2025-11-25")
            .body(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        for sizes in [vec![41_943_090], vec![30 * 1024 * 1024; 2]] {
            let mut input = multi_file_request(&sizes);
            assert!(validate_request(multi_file_request(&sizes), policy).is_ok());
            // A timeout validation error proves transport delivery without
            // starting Pyodide or running any user code in this transport test.
            input.timeout_ms = Some(0);
            let response = post()
                .header("Mcp-Session-Id", &session)
                .header("MCP-Protocol-Version", "2025-11-25")
                .body(serde_json::to_vec(&rpc_request(&input)).unwrap())
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.text().await.unwrap();
            assert!(body.contains("timeout_ms must be between"), "{body}");
        }
        // Size errors return through tools/call before submitting a runtime job.
        for (sizes, message) in [
            (vec![50 * 1024 * 1024 + 1], "per-file limit"),
            (
                vec![30 * 1024 * 1024, 30 * 1024 * 1024 + 1],
                "aggregate limit",
            ),
        ] {
            let input = multi_file_request(&sizes);
            let response = post()
                .header("Mcp-Session-Id", &session)
                .header("MCP-Protocol-Version", "2025-11-25")
                .body(serde_json::to_vec(&rpc_request(&input)).unwrap())
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.text().await.unwrap();
            assert!(body.contains(message), "{body}");
            assert!(body.contains("validation"), "{body}");
        }
        for (extra_bytes, expected_status) in
            [(0, StatusCode::OK), (1, StatusCode::PAYLOAD_TOO_LARGE)]
        {
            let mut body = initialize.as_bytes().to_vec();
            body.resize(policy.max_request_body_bytes() + extra_bytes, b' ');
            let response = post().body(body).send().await.unwrap();
            assert_eq!(response.status(), expected_status);
            if extra_bytes == 1 {
                let body = response.text().await.unwrap();
                assert!(body.contains("HTTP payload too large"), "{body}");
                assert!(
                    body.contains(&policy.max_request_body_bytes().to_string()),
                    "{body}"
                );
                assert!(body.contains("input_limits.max_total_file_kb"), "{body}");
            }
        }
        task.abort();
    }

    #[test]
    fn accepts_nested_file_path() {
        let validated =
            validate_request(request("data/input.bin", b"hello"), default_limits()).unwrap();
        assert_eq!(validated.files[0].path, "data/input.bin");
        assert_eq!(validated.timeout_ms, DEFAULT_TIMEOUT_MS);
    }

    #[test]
    fn rejects_path_traversal_and_absolute_paths() {
        for path in ["../secret", "data/../secret", "/etc/passwd", "a\\b"] {
            assert!(
                validate_request(request(path, b"x"), default_limits()).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn rejects_invalid_base64() {
        let mut input = request("input.bin", b"");
        input.files[0].content_base64 = "not base64".to_owned();
        assert!(validate_request(input, default_limits()).is_err());
    }

    #[test]
    fn advertises_erato_file_metadata() {
        let tool = CodeInterpreterMcpServer::tool_router()
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "run_python_code")
            .unwrap();
        let input_schema = Value::Object(tool.input_schema.as_ref().clone());
        let output_schema = Value::Object(tool.output_schema.unwrap().as_ref().clone());

        let input_content = find_property_schema(&input_schema, "content_base64").unwrap();
        assert_eq!(input_content["contentEncoding"], "base64");
        assert_eq!(input_content["chat.erato/file_content_field"], true);

        let output_file =
            find_object_schema_with_property(&output_schema, "content_base64").unwrap();
        let output_properties = output_file["properties"].as_object().unwrap();
        assert_eq!(
            output_properties["content_base64"]["contentEncoding"],
            "base64"
        );
        assert_eq!(
            output_properties["content_base64"]["chat.erato/file_content_field"],
            true
        );
        assert_eq!(
            output_properties["path"]["chat.erato/file_name_field"],
            true
        );
        assert!(
            output_file["required"]
                .as_array()
                .unwrap()
                .contains(&Value::String("mime_type".to_owned()))
        );
    }

    fn find_property_schema<'a>(schema: &'a Value, property: &str) -> Option<&'a Value> {
        find_object_schema_with_property(schema, property)?["properties"].get(property)
    }

    fn find_object_schema_with_property<'a>(
        schema: &'a Value,
        property: &str,
    ) -> Option<&'a Value> {
        match schema {
            Value::Object(object) => {
                if object
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some_and(|properties| properties.contains_key(property))
                {
                    return Some(schema);
                }
                object
                    .values()
                    .find_map(|value| find_object_schema_with_property(value, property))
            }
            Value::Array(array) => array
                .iter()
                .find_map(|value| find_object_schema_with_property(value, property)),
            _ => None,
        }
    }
}
