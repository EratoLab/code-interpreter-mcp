use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use deno_core::{
    Extension, ExtensionFileSource, JsRuntime, OpState, RuntimeOptions, convert::Uint8Array, op2,
    url::Url, v8,
};
use deno_error::JsErrorBox;

use crate::{RunPythonCodeOutput, RuntimeRequest};
use code_interpreter_mcp::config::AppConfig;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

include!(concat!(env!("OUT_DIR"), "/extension_sources.rs"));

const PYODIDE_ORIGIN: &str = "pyodide.local";
const PYODIDE_VERSION: &str = "314.0.2";
const PACKAGE_CACHE_SCHEMA: u32 = 1;
const MAX_DOWNLOAD_BYTES: usize = 100 * 1024 * 1024;

#[derive(Clone)]
struct RuntimeConfig {
    allowed_hosts: Arc<HashSet<String>>,
    asset_dir: Arc<PathBuf>,
    client: reqwest::Client,
    download_cache: Arc<Mutex<HashMap<String, Arc<Vec<u8>>>>>,
    content_types: Arc<Mutex<HashMap<String, String>>>,
    preinstall_packages: Vec<String>,
    runtime_package_installation: bool,
    package_cache_dir: Arc<PathBuf>,
    package_layer: Option<Arc<PackageLayer>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PackageLayer {
    schema: u32,
    pyodide_version: String,
    preinstall_packages: Vec<String>,
    purelib: String,
    pyodide_packages: Vec<String>,
    files: Vec<PackageLayerFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PackageLayerFile {
    path: String,
    content_base64: String,
}

struct QueuedRequest {
    id: u64,
    request: RuntimeRequest,
    cancelled: Arc<AtomicBool>,
    response: oneshot::Sender<RunPythonCodeOutput>,
}

#[derive(Default)]
struct QueueState {
    requests: VecDeque<QueuedRequest>,
    closed: bool,
}

struct WorkerQueue {
    capacity: usize,
    state: Mutex<QueueState>,
    ready: Condvar,
    next_id: AtomicU64,
}

impl WorkerQueue {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
            next_id: AtomicU64::new(1),
        }
    }

    fn submit(
        &self,
        request: RuntimeRequest,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(u64, oneshot::Receiver<RunPythonCodeOutput>), RuntimeRequest> {
        let mut state = self.state.lock().expect("worker queue mutex poisoned");
        if state.closed || state.requests.len() >= self.capacity {
            return Err(request);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (response, receiver) = oneshot::channel();
        state.requests.push_back(QueuedRequest {
            id,
            request,
            cancelled,
            response,
        });
        self.ready.notify_one();
        Ok((id, receiver))
    }

    fn next(&self) -> Option<QueuedRequest> {
        let mut state = self.state.lock().expect("worker queue mutex poisoned");
        loop {
            if let Some(request) = state.requests.pop_front() {
                return Some(request);
            }
            if state.closed {
                return None;
            }
            state = self.ready.wait(state).expect("worker queue mutex poisoned");
        }
    }

    fn position(&self, id: u64) -> Option<usize> {
        self.state
            .lock()
            .expect("worker queue mutex poisoned")
            .requests
            .iter()
            .position(|request| request.id == id)
            .map(|position| position + 1)
    }

    fn cancel(&self, id: u64) {
        let mut state = self.state.lock().expect("worker queue mutex poisoned");
        if let Some(index) = state.requests.iter().position(|request| request.id == id) {
            if let Some(request) = state.requests.remove(index) {
                request.cancelled.store(true, Ordering::Release);
            }
        }
    }

    fn shutdown(&self) {
        let mut state = self.state.lock().expect("worker queue mutex poisoned");
        state.closed = true;
        state.requests.clear();
        self.ready.notify_all();
    }
}

struct WorkerPoolOwner {
    queue: Arc<WorkerQueue>,
}

impl Drop for WorkerPoolOwner {
    fn drop(&mut self) {
        self.queue.shutdown();
    }
}

struct UserExecutionStarted(bool);

#[op2]
async fn op_fetch_url(
    state: Rc<RefCell<OpState>>,
    #[string] raw_url: String,
) -> Result<Uint8Array, JsErrorBox> {
    let config = {
        let state = state.borrow();
        if state.borrow::<UserExecutionStarted>().0
            && !state.borrow::<RuntimeConfig>().runtime_package_installation
        {
            return Err(JsErrorBox::generic(
                "Runtime package installation is disabled; sandbox downloads are disabled after preinstallation",
            ));
        }
        state.borrow::<RuntimeConfig>().clone()
    };
    if !state.borrow().borrow::<UserExecutionStarted>().0 {
        eprintln!("Python packages: initialization fetching {raw_url} (may use cached artifact)");
    }
    let bytes = fetch_url(&config, &raw_url)
        .await
        .map_err(|error| JsErrorBox::generic(format!("Sandbox fetch failed: {error:#}")))?;
    Ok(bytes.as_ref().clone().into())
}

#[op2]
async fn op_read_package_layer_file(
    state: Rc<RefCell<OpState>>,
    #[string] relative_path: String,
) -> Result<Uint8Array, JsErrorBox> {
    let layer = state
        .borrow()
        .borrow::<RuntimeConfig>()
        .package_layer
        .clone()
        .ok_or_else(|| JsErrorBox::generic("No local package layer is configured"))?;
    let file = layer
        .files
        .iter()
        .find(|file| file.path == relative_path)
        .ok_or_else(|| JsErrorBox::generic("Package layer file is not allowlisted"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&file.content_base64)
        .map_err(|error| JsErrorBox::generic(format!("Invalid package layer file: {error}")))?;
    Ok(bytes.into())
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
fn op_package_log(#[string] message: String) {
    eprintln!("Python packages: {message}");
}

#[op2(fast)]
fn op_random_fill(#[buffer] output: &mut [u8]) -> Result<(), JsErrorBox> {
    getrandom::fill(output)
        .map_err(|error| JsErrorBox::generic(format!("OS randomness failed: {error}")))
}

deno_core::extension!(
    pyodide_host,
    deps = [deno_webidl, deno_web],
    ops = [
        op_fetch_url,
        op_read_package_layer_file,
        op_fetch_content_type,
        op_random_fill,
        op_package_log
    ],
    js = [dir "runtime", "bootstrap.js"],
    options = { config: RuntimeConfig },
    state = |state, options| {
        state.put(options.config);
        state.put(UserExecutionStarted(false)); // User execution has not started.
    },
);

#[derive(Clone)]
pub(crate) struct PyodideRuntime {
    pool: Arc<WorkerPoolOwner>,
}

impl PyodideRuntime {
    pub(crate) async fn new(config: &AppConfig) -> Result<Self> {
        config.validate_worker_settings()?;
        let mut allowed_hosts = config
            .allowed_hosts
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(String::as_str)
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

        let asset_dir = config.asset_dir.clone().unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("node_modules/pyodide")
        });

        let client = reqwest::Client::builder()
            .user_agent(concat!("code-interpreter-mcp/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(45))
            .build()
            .expect("the static HTTP client configuration is valid");

        let mut runtime_config = RuntimeConfig {
            preinstall_packages: config.preinstall_packages.clone(),
            runtime_package_installation: config.runtime_package_installation,
            package_cache_dir: Arc::new(config.package_cache_dir()),
            allowed_hosts: Arc::new(allowed_hosts),
            asset_dir: Arc::new(asset_dir),
            client,
            download_cache: Arc::new(Mutex::new(HashMap::new())),
            content_types: Arc::new(Mutex::new(HashMap::new())),
            package_layer: None,
        };
        ensure_assets(&runtime_config.asset_dir)?;
        if !runtime_config.preinstall_packages.is_empty() {
            runtime_config.package_layer = Some(Arc::new(
                load_or_create_package_layer(runtime_config.clone()).await?,
            ));
        }

        let queue = Arc::new(WorkerQueue::new(config.workers.queue_capacity));
        let (started_tx, mut started_rx) = tokio::sync::mpsc::channel(config.workers.max_workers);
        for worker_id in 0..config.workers.max_workers {
            let queue = queue.clone();
            let worker_config = runtime_config.clone();
            let prewarmed = worker_id < config.workers.prewarmed_workers;
            let started = started_tx.clone();
            let worker_queue = queue.clone();
            if let Err(error) = std::thread::Builder::new()
                .name(format!("pyodide-worker-{worker_id}"))
                .spawn(move || worker_thread(worker_queue, worker_config, prewarmed, started))
            {
                queue.shutdown();
                return Err(error).context("could not start worker thread");
            }
        }
        drop(started_tx);
        for _ in 0..config.workers.max_workers {
            let status = match started_rx.recv().await {
                Some(status) => status,
                None => {
                    queue.shutdown();
                    bail!("worker thread exited during startup");
                }
            };
            if let Err(error) = status {
                queue.shutdown();
                return Err(anyhow::Error::msg(error));
            }
        }

        Ok(Self {
            pool: Arc::new(WorkerPoolOwner { queue }),
        })
    }

    pub(crate) fn submit(
        &self,
        request: RuntimeRequest,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(u64, oneshot::Receiver<RunPythonCodeOutput>), RuntimeRequest> {
        self.pool.queue.submit(request, cancelled)
    }

    #[cfg(test)]
    async fn execute(&self, request: RuntimeRequest) -> RunPythonCodeOutput {
        match self.submit(request, Arc::new(AtomicBool::new(false))) {
            Ok((_, response)) => response.await.unwrap_or_else(|_| {
                RunPythonCodeOutput::runtime("Worker ended before returning a result")
            }),
            Err(_) => RunPythonCodeOutput::runtime("The code execution queue is full"),
        }
    }

    pub(crate) fn queue_position(&self, id: u64) -> Option<usize> {
        self.pool.queue.position(id)
    }

    pub(crate) fn cancel_queued(&self, id: u64) {
        self.pool.queue.cancel(id);
    }
}

fn worker_thread(
    queue: Arc<WorkerQueue>,
    config: RuntimeConfig,
    prewarmed: bool,
    started: tokio::sync::mpsc::Sender<Result<(), String>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = started.blocking_send(Err(format!("could not create worker runtime: {error}")));
            return;
        }
    };
    let mut prepared = if prewarmed {
        match runtime.block_on(create_initialized_isolate(&config, false)) {
            Ok(isolate) => Some(isolate),
            Err(error) => {
                let _ = started.blocking_send(Err(format!("worker prewarm failed: {error:#}")));
                return;
            }
        }
    } else {
        None
    };
    let _ = started.blocking_send(Ok(()));

    loop {
        let Some(job) = queue.next() else {
            break;
        };
        if job.cancelled.load(Ordering::Acquire) {
            continue;
        }
        let timeout_ms = job.request.timeout_ms;
        let result = match prepared.take() {
            Some(isolate) => Ok(isolate),
            None => runtime.block_on(create_initialized_isolate(&config, false)),
        }
        .and_then(|mut isolate| runtime.block_on(execute_python(&mut isolate, &job.request)));
        let output = match result {
            Ok(output) => output,
            Err(error)
                if error
                    .chain()
                    .any(|cause| cause.to_string().contains("execution terminated")) =>
            {
                RunPythonCodeOutput::timed_out(timeout_ms)
            }
            Err(error) => RunPythonCodeOutput::runtime(format!(
                "The embedded Deno/Pyodide runtime failed: {error:#}"
            )),
        };
        let _ = job.response.send(output);

        // The request isolate is discarded. Build a clean replacement before
        // this worker becomes warm for its next request.
        if prewarmed {
            match runtime.block_on(create_initialized_isolate(&config, false)) {
                Ok(isolate) => prepared = Some(isolate),
                Err(error) => {
                    eprintln!("Python worker could not replenish its warm isolate: {error:#}")
                }
            }
        }
    }
}

fn create_runtime(config: &RuntimeConfig) -> Result<JsRuntime> {
    let web_blob_store =
        Arc::new(deno_web::BlobStore::default()) as Arc<dyn deno_web::BlobStoreTrait>;
    let extensions = vec![
        deno_webidl::deno_webidl::init(),
        deno_web::deno_web::init(
            web_blob_store,
            Some(Url::parse("https://pyodide.local/").expect("static URL is valid")),
            false,
            deno_web::InMemoryBroadcastChannel::default(),
        ),
        pyodide_host::init(config.clone()),
    ];
    let extensions = extensions
        .into_iter()
        .map(embed_extension_sources)
        .collect::<Result<Vec<_>>>()?;
    JsRuntime::try_new(RuntimeOptions {
        extensions,
        create_params: Some(v8::Isolate::create_params().heap_limits(0, 512 * 1024 * 1024)),
        ..Default::default()
    })
    .context("could not initialize the embedded Deno runtime")
}

async fn create_initialized_isolate(
    config: &RuntimeConfig,
    build_package_layer: bool,
) -> Result<JsRuntime> {
    let mut runtime = create_runtime(config)?;
    initialize_pyodide(&mut runtime, config, build_package_layer).await?;
    Ok(runtime)
}

async fn initialize_pyodide(
    runtime: &mut JsRuntime,
    config: &RuntimeConfig,
    build_package_layer: bool,
) -> Result<Option<PackageLayer>> {
    let policy = serde_json::json!({
        "preinstall_packages": config.preinstall_packages,
        "runtime_package_installation": config.runtime_package_installation,
        "package_layer": config.package_layer.as_deref().map(|layer| {
            serde_json::json!({
                "purelib": layer.purelib,
                "pyodide_packages": layer.pyodide_packages,
                "files": layer.files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(),
            })
        }),
        "build_package_layer": build_package_layer,
        "pyodide_version": PYODIDE_VERSION,
    });
    runtime.execute_script(
        "<package_policy>",
        format!("globalThis.__packagePolicy = {policy};"),
    )?;
    let asm_source = patched_asm_source(&config.asset_dir)?;
    runtime
        .execute_script("<pyodide.asm.mjs>", asm_source)
        .context("could not evaluate Pyodide's Emscripten module")?;

    let loader_source = fs::read_to_string(config.asset_dir.join("pyodide.js"))
        .context("could not read pyodide.js")?;
    runtime
        .execute_script("<pyodide.js>", loader_source)
        .context("could not evaluate the Pyodide loader")?;

    runtime
        .execute_script(
            "<initialize_pyodide>",
            include_str!("../runtime/initialize.js"),
        )
        .context("could not start Pyodide initialization")?;
    let initialization = runtime
        .execute_script(
            "<get_pyodide_initialization>",
            "globalThis.__pyodideInitialization",
        )
        .context("could not access Pyodide initialization")?;
    #[allow(
        deprecated,
        reason = "resolve_value conveniently drives the isolate event loop"
    )]
    let value = runtime
        .resolve_value(initialization)
        .await
        .context("could not initialize Pyodide and configured packages")?;

    let mut layer = None;
    if build_package_layer {
        deno_core::scope!(scope, runtime);
        let local = v8::Local::new(scope, value);
        let json = local.to_rust_string_lossy(scope);
        layer = Some(serde_json::from_str(&json).context("invalid package layer snapshot")?);
    }
    Ok(layer)
}

async fn execute_python(
    runtime: &mut JsRuntime,
    request: &RuntimeRequest,
) -> Result<RunPythonCodeOutput> {
    runtime
        .op_state()
        .borrow_mut()
        .put(UserExecutionStarted(true));
    let request_json = serde_json::to_string(request).context("could not serialize request")?;
    let quoted_json =
        serde_json::to_string(&request_json).expect("serializing a string cannot fail");
    runtime.execute_script(
        "<set_python_request>",
        format!("globalThis.__request = JSON.parse({quoted_json});"),
    )?;

    let isolate_handle = runtime.v8_isolate().thread_safe_handle();
    let (finished_tx, finished_rx) = mpsc::sync_channel(1);
    let timeout = Duration::from_millis(request.timeout_ms);
    let watchdog = std::thread::spawn(move || {
        if finished_rx.recv_timeout(timeout).is_err() {
            isolate_handle.terminate_execution();
        }
    });
    let result =
        match runtime.execute_script("<execute_python>", include_str!("../runtime/execute.js")) {
            Ok(execution) =>
            {
                #[allow(
                    deprecated,
                    reason = "resolve_value conveniently drives the isolate event loop"
                )]
                runtime
                    .resolve_value(execution)
                    .await
                    .context("Python execution did not resolve")
            }
            Err(error) => Err(anyhow::anyhow!(error.to_string())),
        };
    let _ = finished_tx.send(());
    let _ = watchdog.join();
    let value = result?;
    deno_core::scope!(scope, runtime);
    let local = v8::Local::new(scope, value);
    let json = local.to_rust_string_lossy(scope);
    serde_json::from_str(&json).context("Pyodide returned an invalid result")
}

async fn load_or_create_package_layer(config: RuntimeConfig) -> Result<PackageLayer> {
    let cache_path = config.package_cache_dir.join("package-layer.json");
    if let Ok(bytes) = tokio::fs::read(&cache_path).await
        && let Ok(layer) = serde_json::from_slice::<PackageLayer>(&bytes)
        && package_layer_matches(&layer, &config)
    {
        eprintln!(
            "Python packages: using local package layer {}",
            cache_path.display()
        );
        return Ok(layer);
    }

    eprintln!("Python packages: preparing local package layer at startup");
    let build_config = config.clone();
    let layer = tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("could not create package preparation runtime")?;
        let mut isolate = create_runtime(&build_config)?;
        runtime
            .block_on(initialize_pyodide(&mut isolate, &build_config, true))?
            .context("package preparation did not produce a package layer")
    })
    .await
    .context("package preparation thread failed")??;

    fs::create_dir_all(config.package_cache_dir.as_ref()).with_context(|| {
        format!(
            "could not create package cache {}",
            config.package_cache_dir.display()
        )
    })?;
    let temporary_path = config.package_cache_dir.join("package-layer.json.tmp");
    let serialized = serde_json::to_vec(&layer).context("could not serialize package layer")?;
    fs::write(&temporary_path, serialized)
        .with_context(|| format!("could not write package cache {}", temporary_path.display()))?;
    fs::rename(&temporary_path, &cache_path)
        .with_context(|| format!("could not publish package cache {}", cache_path.display()))?;
    eprintln!("Python packages: local package layer is ready");
    Ok(layer)
}

fn package_layer_matches(layer: &PackageLayer, config: &RuntimeConfig) -> bool {
    layer.schema == PACKAGE_CACHE_SCHEMA
        && layer.pyodide_version == PYODIDE_VERSION
        && layer.preinstall_packages == config.preinstall_packages
        && layer.files.iter().all(|file| {
            let relative = Path::new(&file.path);
            !relative.as_os_str().is_empty()
                && !relative.is_absolute()
                && relative
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
                && base64::engine::general_purpose::STANDARD
                    .decode(&file.content_base64)
                    .is_ok()
        })
}

fn embed_extension_sources(mut extension: Extension) -> Result<Extension> {
    embed_sources(&mut extension.js_files, extension.name)?;
    embed_sources(&mut extension.esm_files, extension.name)?;
    embed_sources(&mut extension.lazy_loaded_esm_files, extension.name)?;
    embed_sources(&mut extension.lazy_loaded_js_files, extension.name)?;
    Ok(extension)
}

fn embed_sources(
    sources: &mut Cow<'static, [ExtensionFileSource]>,
    extension_name: &str,
) -> Result<()> {
    for source in sources.to_mut() {
        if source.is_runtime_loadable() {
            continue;
        }
        let code = EXTENSION_SOURCES
            .iter()
            .find_map(|(specifier, code)| (*specifier == source.specifier).then_some(*code))
            .with_context(|| {
                format!(
                    "no embedded source was compiled for Deno extension {extension_name} file {}",
                    source.specifier
                )
            })?;
        *source = ExtensionFileSource::new_computed(source.specifier, Arc::<str>::from(code));
    }
    Ok(())
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
                "Pyodide asset {} is missing; run `npm install` or set CODE_INTERPRETER_MCP_ASSET_DIR",
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
        let mut relative = PathBuf::new();
        for segment in url
            .path_segments()
            .context("invalid local Pyodide asset path")?
            .filter(|segment| !segment.is_empty())
        {
            relative.push(segment);
        }
        if relative.as_os_str().is_empty()
            || relative.is_absolute()
            || relative.components().count() != 1
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            bail!("local Pyodide asset path is invalid");
        }
        let asset_path = config.asset_dir.join(&relative);
        let root = tokio::fs::canonicalize(config.asset_dir.as_ref())
            .await
            .context("could not resolve local Pyodide asset directory")?;
        let resolved_asset = tokio::fs::canonicalize(&asset_path)
            .await
            .with_context(|| {
                format!(
                    "could not resolve local Pyodide asset {}",
                    relative.display()
                )
            })?;
        if !resolved_asset.starts_with(&root) || !resolved_asset.is_file() {
            bail!(
                "local Pyodide asset is not available: {}",
                relative.display()
            );
        }
        let bytes = tokio::fs::read(resolved_asset).await.with_context(|| {
            format!("could not read local Pyodide asset {}", relative.display())
        })?;
        let content_type = match relative
            .extension()
            .and_then(|extension| extension.to_str())
        {
            Some("json") => "application/json",
            Some("js") => "text/javascript",
            Some("wasm") => "application/wasm",
            _ => "application/octet-stream",
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn run(runtime: &PyodideRuntime, code: &str) -> RunPythonCodeOutput {
        runtime
            .execute(RuntimeRequest {
                code: code.to_owned(),
                files: Vec::new(),
                timeout_ms: 120_000,
            })
            .await
    }

    // These exercise real Pyodide and need npm assets and package-download access.
    #[tokio::test]
    #[ignore = "requires npm ci and HTTPS package downloads"]
    async fn package_policy_in_fresh_sandboxes() {
        let config: AppConfig =
            toml::from_str("preinstall_packages = ['pandas', 'snowballstemmer==3.0.1']\nruntime_package_installation = false")
                .unwrap();
        let runtime = PyodideRuntime::new(&config).await.unwrap();
        for _ in 0..2 {
            let output = run(
                &runtime,
                "import pandas, numpy, snowballstemmer, json\nprint(numpy.arange(3).sum())",
            )
            .await;
            assert!(output.success, "{output:?}");
            assert_eq!(output.stdout, "3\n");
        }
        for code in [
            "import scipy",
            "import micropip\nawait micropip.install('snowballstemmer')",
            "import importlib, micropip\nimportlib.reload(micropip)\nawait micropip.install('snowballstemmer')",
            "import sys, micropip\ndel sys.modules['micropip']\nimport micropip\nawait micropip.install('snowballstemmer')",
            "import pyodide_js\nawait pyodide_js.loadPackage('scipy')",
            "from pyodide.http import pyfetch\nawait pyfetch('https://pypi.org/simple/snowballstemmer/')",
        ] {
            let output = run(&runtime, code).await;
            assert!(!output.success, "{code}: {output:?}");
            assert!(!output.timed_out, "{output:?}");
            let message = &output.error.as_ref().unwrap().message;
            assert!(
                message.contains("disabled")
                    || output
                        .error
                        .as_ref()
                        .unwrap()
                        .traceback
                        .as_deref()
                        .unwrap_or("")
                        .contains("No module named 'scipy'"),
                "{output:?}"
            );
        }
        for source in ["", "runtime_package_installation = true"] {
            let config: AppConfig = toml::from_str(source).unwrap();
            let runtime = PyodideRuntime::new(&config).await.unwrap();
            let output = run(&runtime, "import numpy\nimport micropip\nawait micropip.install('snowballstemmer')\nimport snowballstemmer\nprint(numpy.arange(3).sum())").await;
            assert!(output.success, "{output:?}");
            assert_eq!(output.stdout, "3\n");
        }
        let config: AppConfig =
            toml::from_str("preinstall_packages = ['this-package-does-not-exist-ermcp20']")
                .unwrap();
        let runtime = PyodideRuntime::new(&config).await.unwrap();
        let output = run(&runtime, "print('must not run')").await;
        assert!(!output.success, "{output:?}");
        assert!(output.stdout.is_empty());
        assert!(
            output
                .error
                .unwrap()
                .message
                .contains("this-package-does-not-exist-ermcp20")
        );
    }
}
