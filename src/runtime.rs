use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    env,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use deno_core::{JsRuntime, OpState, RuntimeOptions, convert::Uint8Array, op2, url::Url, v8};
use deno_error::JsErrorBox;

use crate::{RunPythonCodeOutput, RuntimeRequest};

const PYODIDE_ORIGIN: &str = "pyodide.local";
const MAX_DOWNLOAD_BYTES: usize = 100 * 1024 * 1024;
const PYODIDE_ASSETS: &[&str] = &["pyodide-lock.json", "pyodide.asm.wasm", "python_stdlib.zip"];

#[derive(Clone)]
struct RuntimeConfig {
    allowed_hosts: Arc<HashSet<String>>,
    asset_dir: Arc<PathBuf>,
    client: reqwest::Client,
    download_cache: Arc<Mutex<HashMap<String, Arc<Vec<u8>>>>>,
    content_types: Arc<Mutex<HashMap<String, String>>>,
}

#[op2]
async fn op_fetch_url(
    state: Rc<RefCell<OpState>>,
    #[string] raw_url: String,
) -> Result<Uint8Array, JsErrorBox> {
    let config = state.borrow().borrow::<RuntimeConfig>().clone();
    let bytes = fetch_url(&config, &raw_url)
        .await
        .map_err(|error| JsErrorBox::generic(format!("Sandbox fetch failed: {error:#}")))?;
    Ok(bytes.as_ref().clone().into())
}

#[op2]
#[string]
fn op_fetch_content_type(state: &OpState, #[string] raw_url: String) -> String {
    state
        .borrow::<RuntimeConfig>()
        .content_types
        .lock()
        .expect("content-type cache mutex was poisoned")
        .get(&raw_url)
        .cloned()
        .unwrap_or_else(|| "application/octet-stream".to_owned())
}

#[op2(fast)]
fn op_random_fill(#[buffer] output: &mut [u8]) -> Result<(), JsErrorBox> {
    getrandom::fill(output)
        .map_err(|error| JsErrorBox::generic(format!("OS randomness failed: {error}")))
}

deno_core::extension!(
    pyodide_host,
    deps = [deno_webidl, deno_web],
    ops = [op_fetch_url, op_fetch_content_type, op_random_fill],
    js = [dir "runtime", "bootstrap.js"],
    options = { config: RuntimeConfig },
    state = |state, options| {
        state.put(options.config);
    },
);

#[derive(Clone)]
pub(crate) struct PyodideRuntime {
    config: RuntimeConfig,
}

impl PyodideRuntime {
    pub(crate) fn new() -> Self {
        let mut allowed_hosts = env::var("PYODIDE_MCP_ALLOWED_HOSTS")
            .unwrap_or_default()
            .split(',')
            .map(|host| host.trim().to_ascii_lowercase())
            .filter(|host| !host.is_empty())
            .collect::<HashSet<_>>();
        if allowed_hosts.is_empty() {
            allowed_hosts.extend([
                "cdn.jsdelivr.net".to_owned(),
                "files.pythonhosted.org".to_owned(),
                "pypi.org".to_owned(),
            ]);
        }

        let asset_dir = env::var_os("PYODIDE_MCP_ASSET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("node_modules/pyodide")
            });

        let client = reqwest::Client::builder()
            .user_agent(concat!("pyodide-mcp/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(45))
            .build()
            .expect("the static HTTP client configuration is valid");

        Self {
            config: RuntimeConfig {
                allowed_hosts: Arc::new(allowed_hosts),
                asset_dir: Arc::new(asset_dir),
                client,
                download_cache: Arc::new(Mutex::new(HashMap::new())),
                content_types: Arc::new(Mutex::new(HashMap::new())),
            },
        }
    }

    pub(crate) async fn execute(&self, request: RuntimeRequest) -> RunPythonCodeOutput {
        let timeout_ms = request.timeout_ms;
        let config = self.config.clone();
        match tokio::task::spawn_blocking(move || execute_on_isolate(request, config)).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                if error
                    .chain()
                    .any(|cause| cause.to_string().contains("execution terminated"))
                {
                    RunPythonCodeOutput::timed_out(timeout_ms)
                } else {
                    RunPythonCodeOutput::runtime(format!(
                        "The embedded Deno/Pyodide runtime failed: {error:#}"
                    ))
                }
            }
            Err(error) => RunPythonCodeOutput::runtime(format!(
                "The embedded Deno runtime thread failed: {error}"
            )),
        }
    }
}

fn execute_on_isolate(
    request: RuntimeRequest,
    config: RuntimeConfig,
) -> Result<RunPythonCodeOutput> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not create the isolate's Tokio runtime")?;
    runtime.block_on(execute_async(request, config))
}

async fn execute_async(
    request: RuntimeRequest,
    config: RuntimeConfig,
) -> Result<RunPythonCodeOutput> {
    ensure_assets(&config.asset_dir)?;

    let web_blob_store =
        Arc::new(deno_web::BlobStore::default()) as Arc<dyn deno_web::BlobStoreTrait>;
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: vec![
            deno_webidl::deno_webidl::init(),
            deno_web::deno_web::init(
                web_blob_store,
                Some(Url::parse("https://pyodide.local/").expect("static URL is valid")),
                false,
                deno_web::InMemoryBroadcastChannel::default(),
            ),
            pyodide_host::init(config.clone()),
        ],
        create_params: Some(v8::Isolate::create_params().heap_limits(0, 512 * 1024 * 1024)),
        ..Default::default()
    });

    let isolate_handle = runtime.v8_isolate().thread_safe_handle();
    let (finished_tx, finished_rx) = mpsc::sync_channel(1);
    let execution_timeout = Duration::from_millis(request.timeout_ms);
    let watchdog = std::thread::spawn(move || {
        if finished_rx.recv_timeout(execution_timeout).is_err() {
            isolate_handle.terminate_execution();
        }
    });

    let result = run_pyodide(&mut runtime, &request, &config.asset_dir).await;
    let _ = finished_tx.send(());
    let _ = watchdog.join();
    result
}

async fn run_pyodide(
    runtime: &mut JsRuntime,
    request: &RuntimeRequest,
    asset_dir: &Path,
) -> Result<RunPythonCodeOutput> {
    let asm_source = patched_asm_source(asset_dir)?;
    runtime
        .execute_script("<pyodide.asm.mjs>", asm_source)
        .context("could not evaluate Pyodide's Emscripten module")?;

    let loader_source = std::fs::read_to_string(asset_dir.join("pyodide.js"))
        .context("could not read pyodide.js")?;
    runtime
        .execute_script("<pyodide.js>", loader_source)
        .context("could not evaluate the Pyodide loader")?;

    let initialization = runtime
        .execute_script(
            "<initialize_pyodide>",
            include_str!("../runtime/initialize.js"),
        )
        .context("could not start Pyodide initialization")?;
    #[allow(
        deprecated,
        reason = "resolve_value conveniently drives the isolate event loop"
    )]
    runtime
        .resolve_value(initialization)
        .await
        .context("could not initialize Pyodide and micropip")?;

    let request_json = serde_json::to_string(request).context("could not serialize request")?;
    let quoted_json =
        serde_json::to_string(&request_json).expect("serializing a string cannot fail");
    runtime
        .execute_script(
            "<set_python_request>",
            format!("globalThis.__request = JSON.parse({quoted_json});"),
        )
        .context("could not pass the request to Pyodide")?;

    let execution = runtime
        .execute_script("<execute_python>", include_str!("../runtime/execute.js"))
        .context("could not start Python execution")?;
    #[allow(
        deprecated,
        reason = "resolve_value conveniently drives the isolate event loop"
    )]
    let value = runtime
        .resolve_value(execution)
        .await
        .context("Python execution did not resolve")?;

    deno_core::scope!(scope, runtime);
    let local = v8::Local::new(scope, value);
    let json = local.to_rust_string_lossy(scope);
    serde_json::from_str(&json).context("Pyodide returned an invalid result")
}

fn patched_asm_source(asset_dir: &Path) -> Result<String> {
    let mut source = std::fs::read_to_string(asset_dir.join("pyodide.asm.mjs"))
        .context("could not read pyodide.asm.mjs")?;
    let import_meta_count = source.matches("import.meta.url").count();
    if import_meta_count != 3 {
        bail!(
            "unexpected pyodide.asm.mjs shape: expected 3 import.meta.url references, found {import_meta_count}"
        );
    }
    source = source.replace(
        "import.meta.url",
        "\"https://pyodide.local/pyodide.asm.mjs\"",
    );

    const EXPORT: &str = "export default _createPyodideModule;";
    if !source.contains(EXPORT) {
        bail!("unexpected pyodide.asm.mjs shape: default export was not found");
    }
    Ok(source.replace(
        EXPORT,
        "globalThis.createPyodideModule = _createPyodideModule;",
    ))
}

fn ensure_assets(asset_dir: &Path) -> Result<()> {
    for asset in [
        "pyodide.js",
        "pyodide.asm.mjs",
        "pyodide.asm.wasm",
        "pyodide-lock.json",
        "python_stdlib.zip",
    ] {
        let path = asset_dir.join(asset);
        if !path.is_file() {
            bail!(
                "Pyodide asset {} is missing; run `npm install` or set PYODIDE_MCP_ASSET_DIR",
                path.display()
            );
        }
    }
    Ok(())
}

async fn fetch_url(config: &RuntimeConfig, raw_url: &str) -> Result<Arc<Vec<u8>>> {
    let url = Url::parse(raw_url).with_context(|| format!("invalid URL: {raw_url}"))?;
    let host = url
        .host_str()
        .context("network URL does not contain a host")?
        .to_ascii_lowercase();

    if url.scheme() != "https" {
        bail!("only HTTPS URLs are allowed");
    }

    if host == PYODIDE_ORIGIN {
        let mut segments = url
            .path_segments()
            .context("invalid local Pyodide asset path")?
            .filter(|segment| !segment.is_empty());
        let asset = segments.next().context("local asset path is empty")?;
        if segments.next().is_some() || !PYODIDE_ASSETS.contains(&asset) {
            bail!("local Pyodide asset is not allowlisted: {asset}");
        }
        let bytes = tokio::fs::read(config.asset_dir.join(asset))
            .await
            .with_context(|| format!("could not read local Pyodide asset {asset}"))?;
        let content_type = if asset.ends_with(".json") {
            "application/json"
        } else {
            "application/octet-stream"
        };
        config
            .content_types
            .lock()
            .expect("content-type cache mutex was poisoned")
            .insert(raw_url.to_owned(), content_type.to_owned());
        return Ok(Arc::new(bytes));
    }

    if !config
        .allowed_hosts
        .iter()
        .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
    {
        bail!("network host is not allowlisted: {host}");
    }

    if let Some(cached) = config
        .download_cache
        .lock()
        .expect("download cache mutex was poisoned")
        .get(raw_url)
        .cloned()
    {
        return Ok(cached);
    }

    let mut response = config
        .client
        .get(url)
        .header(
            reqwest::header::ACCEPT,
            "application/vnd.pypi.simple.v1+json, */*;q=0.01",
        )
        .send()
        .await
        .with_context(|| format!("request to {host} failed"))?
        .error_for_status()
        .with_context(|| format!("request to {host} returned an error status"))?;
    if response.content_length().unwrap_or(0) > MAX_DOWNLOAD_BYTES as u64 {
        bail!("download exceeds the {MAX_DOWNLOAD_BYTES} byte limit");
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or(0)
            .min(MAX_DOWNLOAD_BYTES as u64) as usize,
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("could not read response from {host}"))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_DOWNLOAD_BYTES {
            bail!("download exceeds the {MAX_DOWNLOAD_BYTES} byte limit");
        }
        bytes.extend_from_slice(&chunk);
    }

    let bytes = Arc::new(bytes);
    config
        .download_cache
        .lock()
        .expect("download cache mutex was poisoned")
        .insert(raw_url.to_owned(), bytes.clone());
    config
        .content_types
        .lock()
        .expect("content-type cache mutex was poisoned")
        .insert(raw_url.to_owned(), content_type);
    Ok(bytes)
}
