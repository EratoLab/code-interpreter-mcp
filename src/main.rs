use std::{
    collections::HashSet,
    env,
    io::{self, Write},
    net::SocketAddr,
    path::{Component, Path},
};

use anyhow::{Context, Result, anyhow, bail};
use axum::Router;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use rmcp::{
    Json, ServerHandler,
    handler::server::wrapper::Parameters,
    schemars, tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::{Deserialize, Serialize};

mod runtime;
use runtime::PyodideRuntime;

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 120_000;
const MAX_CODE_BYTES: usize = 1024 * 1024;
const MAX_FILES: usize = 64;
const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;
const MAX_TOTAL_FILE_BYTES: usize = 50 * 1024 * 1024;
const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8000";

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
}

#[tool_router]
impl CodeInterpreterMcpServer {
    fn new() -> Self {
        Self {
            runtime: PyodideRuntime::new(),
        }
    }

    #[tool(
        description = "Run Python code in a fresh Pyodide WebAssembly environment. Supports top-level await, pandas and other Pyodide packages, micropip.install() for compatible Python wheels, and Base64-encoded input files mounted below /workspace. Returns captured stdout, stderr, the final expression, Base64-encoded files created by the script, and a structured error when execution fails."
    )]
    async fn run_python_code(
        &self,
        Parameters(request): Parameters<RunPythonCodeRequest>,
    ) -> Json<RunPythonCodeOutput> {
        let request = match validate_request(request) {
            Ok(request) => request,
            Err(error) => {
                return Json(RunPythonCodeOutput::validation(error.to_string()));
            }
        };

        Json(self.runtime.execute(request).await)
    }
}

#[tool_handler(
    name = "code-interpreter-mcp",
    version = "0.1.0",
    instructions = "Use run_python_code to execute Python in a fresh Pyodide WebAssembly sandbox."
)]
impl ServerHandler for CodeInterpreterMcpServer {}

fn validate_request(request: RunPythonCodeRequest) -> Result<RuntimeRequest> {
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

        let max_encoded_len = MAX_FILE_BYTES.div_ceil(3) * 4;
        if file.content_base64.len() > max_encoded_len {
            bail!("file {path} exceeds the {MAX_FILE_BYTES} byte limit");
        }
        let decoded = BASE64
            .decode(file.content_base64.as_bytes())
            .with_context(|| format!("file {path} is not valid canonical Base64"))?;
        if decoded.len() > MAX_FILE_BYTES {
            bail!("file {path} exceeds the {MAX_FILE_BYTES} byte limit");
        }
        total_bytes = total_bytes
            .checked_add(decoded.len())
            .ok_or_else(|| anyhow!("total file size overflowed"))?;
        if total_bytes > MAX_TOTAL_FILE_BYTES {
            bail!("files exceed the {MAX_TOTAL_FILE_BYTES} byte total limit");
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

#[tokio::main]
async fn main() -> Result<()> {
    let configured_address = env::var("CODE_INTERPRETER_MCP_BIND_ADDR")
        .unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_owned());
    let bind_address: SocketAddr = configured_address
        .parse()
        .with_context(|| format!("invalid CODE_INTERPRETER_MCP_BIND_ADDR: {configured_address}"))?;

    let server = CodeInterpreterMcpServer::new();
    let http_service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    let router = Router::new().nest_service("/mcp", http_service);
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

    axum::serve(listener, router)
        .await
        .context("MCP Streamable HTTP server failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

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

    #[test]
    fn accepts_nested_file_path() {
        let validated = validate_request(request("data/input.bin", b"hello")).unwrap();
        assert_eq!(validated.files[0].path, "data/input.bin");
        assert_eq!(validated.timeout_ms, DEFAULT_TIMEOUT_MS);
    }

    #[test]
    fn rejects_path_traversal_and_absolute_paths() {
        for path in ["../secret", "data/../secret", "/etc/passwd", "a\\b"] {
            assert!(validate_request(request(path, b"x")).is_err(), "{path}");
        }
    }

    #[test]
    fn rejects_invalid_base64() {
        let mut input = request("input.bin", b"");
        input.files[0].content_base64 = "not base64".to_owned();
        assert!(validate_request(input).is_err());
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
