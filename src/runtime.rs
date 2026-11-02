use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
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
use code_interpreter_mcp::metrics as app_metrics;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

include!(concat!(env!("OUT_DIR"), "/extension_sources.rs"));

const PYODIDE_ORIGIN: &str = "pyodide.local";
const PYODIDE_VERSION: &str = "314.0.2";
const PYODIDE_PACKAGE_CDN: &str = "cdn.jsdelivr.net";
const PACKAGE_CACHE_SCHEMA: u32 = 1;
const MAX_DOWNLOAD_BYTES: usize = 100 * 1024 * 1024;

// A fixed reserve, granted at most once, lets V8 finish its current allocation
// and observe termination. It is not an additional execution budget.
const EMERGENCY_HEAP_BYTES: usize = 64 * 1024 * 1024;
const INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(300);
const MEMORY: usize = 1;
const TIMEOUT: usize = 2;
const CANCELLED: usize = 3;

#[derive(Clone)]
struct Termination {
    cause: Arc<AtomicUsize>,
    memory_limit_bytes: usize,
}

impl Termination {
    fn error(&self, phase: &str) -> Option<anyhow::Error> {
        let cause = self.cause.load(Ordering::Acquire);
        let message = match cause {
            MEMORY => format!(
                "V8 memory limit of {} MiB reached during {phase}; execution was terminated and the isolate discarded",
                self.memory_limit_bytes / (1024 * 1024)
            ),
            TIMEOUT => format!("{phase} exceeded its time limit"),
            CANCELLED => format!("{phase} was cancelled"),
            _ => return None,
        };
        Some(anyhow::anyhow!(TerminationError { cause, message }))
    }
}

#[derive(Debug)]
struct TerminationError {
    cause: usize,
    message: String,
}

impl std::fmt::Display for TerminationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for TerminationError {}

// Own and join the watchdog on every exit, including early errors and unwinds.
struct ExecutionWatchdog {
    finished: mpsc::SyncSender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ExecutionWatchdog {
    fn start(
        runtime: &mut JsRuntime,
        timeout: Duration,
        cancelled: Arc<AtomicBool>,
    ) -> (Self, Termination) {
        let termination = runtime.op_state().borrow().borrow::<Termination>().clone();
        let cause = termination.cause.clone();
        let handle = runtime.v8_isolate().thread_safe_handle();
        let (finished, receiver) = mpsc::sync_channel(1);
        let deadline = Instant::now() + timeout;
        let thread = std::thread::spawn(move || {
            loop {
                let reason = if cancelled.load(Ordering::Acquire) {
                    CANCELLED
                } else if Instant::now() >= deadline {
                    TIMEOUT
                } else {
                    0
                };
                if reason != 0 {
                    let _ = cause.compare_exchange(0, reason, Ordering::AcqRel, Ordering::Acquire);
                    handle.terminate_execution();
                    break;
                }
                match receiver.recv_timeout(
                    Duration::from_millis(10)
                        .min(deadline.saturating_duration_since(Instant::now())),
                ) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    _ => break,
                }
            }
        });
        (
            Self {
                finished,
                thread: Some(thread),
            },
            termination,
        )
    }
}

impl Drop for ExecutionWatchdog {
    fn drop(&mut self) {
        let _ = self.finished.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone)]
struct RuntimeConfig {
    memory_limit_bytes: usize,
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
    enqueued: Instant,
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

/// Probe reads never acquire the queue mutex or depend on Python execution.
#[derive(Default)]
pub(crate) struct WorkerHealth {
    accepting: AtomicBool,
    live_workers: AtomicUsize,
    fatal: AtomicBool,
}

impl WorkerHealth {
    pub(crate) fn is_live(&self) -> bool {
        !self.fatal.load(Ordering::Acquire)
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.accepting.load(Ordering::Acquire) && self.live_workers() > 0 && self.is_live()
    }

    pub(crate) fn live_workers(&self) -> usize {
        self.live_workers.load(Ordering::Acquire)
    }
}

/// Lives for the entire worker thread, including startup and panic unwinding.
pub(crate) struct WorkerGuard(Arc<WorkerQueue>, app_metrics::WorkerState);

impl WorkerGuard {
    fn new(queue: Arc<WorkerQueue>) -> Self {
        queue.health.live_workers.fetch_add(1, Ordering::AcqRel);
        Self(queue, app_metrics::WorkerState::new())
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if !self.0.stopping.load(Ordering::Acquire) || std::thread::panicking() {
            metrics::counter!(app_metrics::EXITS).increment(1);
        }
        let last = self.0.health.live_workers.fetch_sub(1, Ordering::AcqRel) == 1;
        if self.0.state.is_poisoned() || (last && !self.0.stopping.load(Ordering::Acquire)) {
            self.0.health.fatal.store(true, Ordering::Release);
            self.0.shutdown();
        }
    }
}

struct WorkerQueue {
    capacity: usize,
    health: Arc<WorkerHealth>,
    stopping: AtomicBool,
    state: Mutex<QueueState>,
    ready: Condvar,
    next_id: AtomicU64,
}

impl WorkerQueue {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            health: Arc::new(WorkerHealth::default()),
            stopping: AtomicBool::new(false),
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
        if !self.health.is_ready() {
            app_metrics::rejection("shutdown");
            return Err(request);
        }
        let Ok(mut state) = self.state.lock() else {
            app_metrics::rejection("shutdown");
            return Err(request);
        };
        if state.closed || !self.health.is_ready() {
            app_metrics::rejection("shutdown");
            return Err(request);
        }
        if state.requests.len() >= self.capacity {
            app_metrics::rejection("queue_full");
            return Err(request);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (response, receiver) = oneshot::channel();
        metrics::gauge!(app_metrics::DEPTH).increment(1.);
        state.requests.push_back(QueuedRequest {
            enqueued: Instant::now(),
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
                let cancelled =
                    request.cancelled.load(Ordering::Acquire) || request.response.is_closed();
                app_metrics::queue_removed(
                    request.enqueued,
                    if cancelled { "cancelled" } else { "dispatched" },
                );
                if cancelled {
                    continue;
                }
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
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(index) = state.requests.iter().position(|request| request.id == id)
            && let Some(request) = state.requests.remove(index)
        {
            app_metrics::queue_removed(request.enqueued, "cancelled");
            request.cancelled.store(true, Ordering::Release);
        }
    }

    fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        self.health.accepting.store(false, Ordering::Release);
        // Recover poison here so a worker's unwind guard cannot double-panic.
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        for job in state.requests.drain(..) {
            app_metrics::queue_removed(job.enqueued, "shutdown");
            let _ = job.response.send(RunPythonCodeOutput::runtime(
                "Worker pool stopped before queued execution could start",
            ));
        }
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

// Request-local buffers are consumed once and moved into V8 backing stores.
struct InputFileBuffers(Vec<Option<Vec<u8>>>);

#[op2]
fn op_take_input_file(state: &mut OpState, index: u32) -> Result<Uint8Array, JsErrorBox> {
    state
        .borrow_mut::<InputFileBuffers>()
        .0
        .get_mut(index as usize)
        .and_then(Option::take)
        .map(Into::into)
        .ok_or_else(|| JsErrorBox::generic("Input file is unavailable or already consumed"))
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
        op_take_input_file,
        op_read_package_layer_file,
        op_fetch_content_type,
        op_random_fill,
        op_package_log
    ],
    js = [dir "runtime", "bootstrap.js"],
    options = { config: RuntimeConfig },
    state = |state, options| {
        state.put(options.config);
        state.put(InputFileBuffers(Vec::new()));
        state.put(UserExecutionStarted(false)); // User execution has not started.
    },
);

#[derive(Clone)]
pub(crate) struct PyodideRuntime {
    pool: Arc<WorkerPoolOwner>,
}

impl PyodideRuntime {
    #[cfg(test)]
    pub(crate) fn test_pool() -> (Self, WorkerGuard) {
        let queue = Arc::new(WorkerQueue::new(1));
        let guard = WorkerGuard::new(queue.clone());
        queue.health.accepting.store(true, Ordering::Release);
        (
            Self {
                pool: Arc::new(WorkerPoolOwner { queue }),
            },
            guard,
        )
    }

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
            memory_limit_bytes: config.workers.memory_limit_bytes()?,
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

        queue.health.accepting.store(true, Ordering::Release);
        if !queue.health.is_ready() {
            queue.shutdown();
            bail!("worker pool failed during startup");
        }
        Ok(Self {
            pool: Arc::new(WorkerPoolOwner { queue }),
        })
    }

    pub(crate) fn health(&self) -> Arc<WorkerHealth> {
        self.pool.queue.health.clone()
    }

    pub(crate) fn shutdown(&self) {
        self.pool.queue.shutdown();
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
    let mut guard = WorkerGuard::new(queue.clone());
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
        match initialize_worker(&runtime, &config, &mut guard.1, "startup") {
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
    drop(started);

    loop {
        guard.1.set("idle");
        let Some(job) = queue.next() else {
            break;
        };
        if job.cancelled.load(Ordering::Acquire) {
            continue;
        }
        let result = match prepared.take() {
            Some(isolate) => Ok(isolate),
            None => initialize_worker(&runtime, &config, &mut guard.1, "on_demand"),
        }
        .and_then(|mut isolate| {
            guard.1.set("executing");
            let start = Instant::now();
            let result = runtime.block_on(execute_python(
                &mut isolate,
                job.request,
                job.cancelled.clone(),
            ));
            let outcome = match &result {
                Ok(output) => output.outcome(),
                Err(_) => "runtime_failure",
            };
            metrics::histogram!(app_metrics::EXECUTION, "outcome" => outcome)
                .record(start.elapsed().as_secs_f64());
            result
        });
        let output = match result {
            Ok(output) => output,
            Err(error) => termination_output(error),
        };
        let _ = job.response.send(output);

        // The request isolate is discarded. Build a clean replacement before
        // this worker becomes warm for its next request.
        if prewarmed && !queue.stopping.load(Ordering::Acquire) {
            match initialize_worker(&runtime, &config, &mut guard.1, "replenishment") {
                Ok(isolate) => prepared = Some(isolate),
                Err(error) => {
                    eprintln!("Python worker could not replenish its warm isolate: {error:#}")
                }
            }
        }
    }
}

fn initialize_worker(
    runtime: &tokio::runtime::Runtime,
    config: &RuntimeConfig,
    state: &mut app_metrics::WorkerState,
    phase: &'static str,
) -> Result<JsRuntime> {
    state.set("initializing");
    let start = Instant::now();
    let result = runtime.block_on(create_initialized_isolate(config, false));
    metrics::histogram!(app_metrics::INIT, "phase" => phase, "outcome" => if result.is_ok() { "success" } else { "runtime_failure" }).record(start.elapsed().as_secs_f64());
    result
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
    let mut runtime = JsRuntime::try_new(RuntimeOptions {
        extensions,
        create_params: Some(v8::Isolate::create_params().heap_limits(0, config.memory_limit_bytes)),
        ..Default::default()
    })
    .context("could not initialize the embedded Deno runtime")?;
    let termination = Termination {
        cause: Arc::new(AtomicUsize::new(0)),
        memory_limit_bytes: config.memory_limit_bytes,
    };
    let cause = termination.cause.clone();
    let handle = runtime.v8_isolate().thread_safe_handle();
    // deno_core owns this closure until isolate disposal. No allocation, logging,
    // or JS re-entry is permitted here. Memory takes precedence over a racing timer.
    // Callback limits are old-generation sizes, not total heap statistics.
    // Anchor the reserve to the initial limit so repeated calls cannot grow it.
    runtime.add_near_heap_limit_callback(move |current_limit, initial_limit| {
        cause.store(MEMORY, Ordering::Release);
        handle.terminate_execution();
        current_limit.max(initial_limit.saturating_add(EMERGENCY_HEAP_BYTES))
    });
    runtime.op_state().borrow_mut().put(termination);
    Ok(runtime)
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
    let (watchdog, termination) = ExecutionWatchdog::start(
        runtime,
        INITIALIZATION_TIMEOUT,
        Arc::new(AtomicBool::new(false)),
    );
    let result = tokio::time::timeout(
        INITIALIZATION_TIMEOUT,
        initialize_pyodide_inner(runtime, config, build_package_layer),
    )
    .await;
    if result.is_err() {
        let _ = termination
            .cause
            .compare_exchange(0, TIMEOUT, Ordering::AcqRel, Ordering::Acquire);
    }
    drop(watchdog);
    if let Some(error) = termination.error("Pyodide/package initialization") {
        return Err(error);
    }
    result.context("Pyodide initialization timed out")?
}

async fn initialize_pyodide_inner(
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

fn termination_output(error: anyhow::Error) -> RunPythonCodeOutput {
    if let Some(termination) = error.downcast_ref::<TerminationError>() {
        return match termination.cause {
            MEMORY => RunPythonCodeOutput::failure(
                "memory_limit",
                termination.message.clone(),
                None,
                false,
            ),
            TIMEOUT => {
                RunPythonCodeOutput::failure("timeout", termination.message.clone(), None, true)
            }
            CANCELLED => {
                RunPythonCodeOutput::failure("cancelled", termination.message.clone(), None, false)
            }
            _ => unreachable!(),
        };
    }
    RunPythonCodeOutput::runtime(format!(
        "The embedded Deno/Pyodide runtime failed: {error:#}"
    ))
}

async fn execute_python(
    runtime: &mut JsRuntime,
    request: RuntimeRequest,
    cancelled: Arc<AtomicBool>,
) -> Result<RunPythonCodeOutput> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(RunPythonCodeOutput::failure(
            "cancelled",
            "Request cancelled",
            None,
            false,
        ));
    }
    let timeout_ms = request.timeout_ms;
    let timeout = Duration::from_millis(timeout_ms);
    let (watchdog, termination) = ExecutionWatchdog::start(runtime, timeout, cancelled.clone());
    let result = tokio::select! {
        result = tokio::time::timeout(timeout, execute_python_inner(runtime, request)) => {
            match result {
                Ok(result) => result,
                Err(_) => {
                    let _ = termination.cause.compare_exchange(0, TIMEOUT, Ordering::AcqRel, Ordering::Acquire);
                    Err(anyhow::anyhow!("execution timed out"))
                }
            }
        }
        _ = async {
            while !cancelled.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        } => {
            let _ = termination.cause.compare_exchange(0, CANCELLED, Ordering::AcqRel, Ordering::Acquire);
            Err(anyhow::anyhow!("execution cancelled"))
        }
    };
    drop(watchdog);
    if termination.cause.load(Ordering::Acquire) == TIMEOUT {
        return Ok(RunPythonCodeOutput::timed_out(timeout_ms));
    }
    if let Some(error) = termination.error("request injection or execution") {
        return Ok(termination_output(error));
    }
    result
}

async fn execute_python_inner(
    runtime: &mut JsRuntime,
    mut request: RuntimeRequest,
) -> Result<RunPythonCodeOutput> {
    runtime
        .op_state()
        .borrow_mut()
        .put(UserExecutionStarted(true));
    let buffers = request
        .files
        .iter_mut()
        .map(|file| Some(std::mem::take(&mut file.bytes)))
        .collect();
    runtime
        .op_state()
        .borrow_mut()
        .put(InputFileBuffers(buffers));
    let request_json = serde_json::to_string(&request).context("could not serialize request")?;
    let quoted_json =
        serde_json::to_string(&request_json).expect("serializing a string cannot fail");
    runtime.execute_script(
        "<set_python_request>",
        format!("globalThis.__request = JSON.parse({quoted_json});"),
    )?;

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
        let resolved_asset = match tokio::fs::canonicalize(&asset_path).await {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // The npm Pyodide package contains the runtime, but not the
                // wheel files referenced by pyodide-lock.json. Fetch those
                // from the matching, versioned Pyodide CDN distribution.
                let cdn_url = format!(
                    "https://{PYODIDE_PACKAGE_CDN}/pyodide/v{PYODIDE_VERSION}/full/{}",
                    relative.display()
                );
                return Box::pin(fetch_url(config, &cdn_url))
                    .await
                    .with_context(|| {
                        format!(
                            "local Pyodide asset {} is missing and its CDN fallback failed",
                            relative.display()
                        )
                    });
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "could not resolve local Pyodide asset {}",
                        relative.display()
                    )
                });
            }
        };
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

    fn pressure_config(memory_limit_mb: usize) -> RuntimeConfig {
        RuntimeConfig {
            memory_limit_bytes: memory_limit_mb * 1024 * 1024,
            allowed_hosts: Arc::new(HashSet::new()),
            asset_dir: Arc::new(PathBuf::new()),
            client: reqwest::Client::new(),
            download_cache: Arc::new(Mutex::new(HashMap::new())),
            content_types: Arc::new(Mutex::new(HashMap::new())),
            preinstall_packages: Vec::new(),
            runtime_package_installation: false,
            package_cache_dir: Arc::new(PathBuf::new()),
            package_layer: None,
        }
    }

    // V8 aborts cannot be caught by Rust: all pressure cases run in a child.
    #[test]
    fn heap_pressure_child_process() {
        run_child_test(
            "runtime::tests::heap_pressure_child",
            Duration::from_secs(90),
        );
    }

    fn run_child_test(name: &str, timeout: Duration) {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--ignored", "--nocapture"])
            .spawn()
            .unwrap();
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "heap-pressure child failed: {status}");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("heap-pressure child exceeded {timeout:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[tokio::test]
    #[ignore = "potentially fatal; invoked by heap_pressure_child_process"]
    async fn heap_pressure_child() {
        let config = pressure_config(32);
        // Exercise repeated replacement creation and a JS allocation loop
        // independent of base64 decoding and Python/Wasm allocations.
        for _ in 0..2 {
            let mut runtime = create_runtime(&config).unwrap();
            let (watchdog, termination) = ExecutionWatchdog::start(
                &mut runtime,
                Duration::from_secs(20),
                Arc::new(AtomicBool::new(false)),
            );
            assert!(runtime.execute_script("<pressure>",
                "globalThis.retained = []; while (true) retained.push(new Array(10000).fill(123));"
            ).is_err());
            drop(watchdog);
            let output = termination_output(termination.error("allocation test").unwrap());
            assert_eq!(output.error.unwrap().kind, "memory_limit");
            assert!(!output.timed_out);
            let actual_limit = runtime.v8_isolate().get_heap_statistics().heap_size_limit();
            assert!(
                actual_limit <= config.memory_limit_bytes + EMERGENCY_HEAP_BYTES,
                "actual heap limit: {actual_limit}"
            );
            drop(runtime);
            let mut replacement = create_runtime(&config).unwrap();
            replacement.execute_script("<small>", "1 + 1").unwrap();
        }

        // The same guarded initializer serves startup, package-layer building,
        // on-demand creation and warm replacement.
        let assets = std::env::temp_dir().join(format!("ermcp-pressure-{}", std::process::id()));
        fs::create_dir_all(&assets).unwrap();
        fs::write(assets.join("pyodide.asm.mjs"),
            "// import.meta.url import.meta.url import.meta.url\nconst _createPyodideModule = null; export default _createPyodideModule; globalThis.retained = []; while(true) retained.push(new Array(10000).fill(123));"
        ).unwrap();
        let mut init_config = config.clone();
        init_config.asset_dir = Arc::new(assets.clone());
        for build_layer in [false, true] {
            let mut runtime = create_runtime(&init_config).unwrap();
            let error = initialize_pyodide(&mut runtime, &init_config, build_layer)
                .await
                .unwrap_err();
            assert_eq!(
                error.downcast_ref::<TerminationError>().unwrap().cause,
                MEMORY
            );
            assert!(error.to_string().contains("initialization"));
        }
        fs::remove_dir_all(assets).unwrap();

        // Allocation during request injection must be classified too.
        let mut runtime = create_runtime(&config).unwrap();
        let mut large = request();
        large.code = "x".repeat(40 * 1024 * 1024);
        large.timeout_ms = 20_000;
        let output = execute_python(&mut runtime, large, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(output.error.unwrap().kind, "memory_limit");
        assert!(!output.timed_out);
        drop(runtime);

        // Use a JS stand-in to exercise synchronous termination and pending
        // promises without depending on Pyodide's network assets.
        for (code, cancelled, expected) in [
            ("while (true) {}", false, "timeout"),
            (
                "return new Promise(resolve => setTimeout(resolve, 10000))",
                false,
                "timeout",
            ),
            ("while (true) {}", true, "cancelled"),
        ] {
            let mut runtime = create_runtime(&config).unwrap();
            runtime.execute_script("<stub>", format!(
                "globalThis.__resetCapture = () => {{}}; globalThis.__packagePolicy = {{runtime_package_installation:false}}; globalThis.__pyodide = {{FS: {{mkdirTree(){{}}, chdir(){{}}, readdir(){{return [];}}}}, runPythonAsync: () => {{{code}}}}};"
            )).unwrap();
            let mut req = request();
            req.timeout_ms = if cancelled { 1000 } else { 30 };
            let cancellation = Arc::new(AtomicBool::new(false));
            let signal = cancellation.clone();
            let canceller = cancelled.then(|| {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(10));
                    signal.store(true, Ordering::Release);
                })
            });
            let output = execute_python(&mut runtime, req, cancellation)
                .await
                .unwrap();
            if let Some(canceller) = canceller {
                canceller.join().unwrap();
            }
            assert_eq!(output.error.unwrap().kind, expected);
            assert_eq!(output.timed_out, expected == "timeout");
        }
    }

    // Python objects live in Wasm memory, outside the V8 heap budget. Keep
    // V8 arrays alive through Python's JS bridge to exercise that budget from
    // user code without depending on the file mounting implementation.
    const PYTHON_HEAP_PRESSURE: &str = r#"
from pyodide.ffi import to_js
chunk = [123] * 100_000
retained = []
while True:
    retained.append(to_js(chunk))
"#;

    #[test]
    #[ignore = "requires npm assets and the micropip wheel"]
    fn python_heap_pressure_child_process() {
        run_child_test(
            "runtime::tests::python_heap_pressure_child",
            Duration::from_secs(180),
        );
    }

    #[tokio::test]
    #[ignore = "potentially fatal; invoked by python_heap_pressure_child_process"]
    async fn python_heap_pressure_child() {
        if std::env::var_os("ERMCP_HEAP_BASELINE").is_some() {
            let mut config = pressure_config(512);
            config.asset_dir =
                Arc::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("node_modules/pyodide"));
            let mut runtime = create_initialized_isolate(&config, false).await.unwrap();
            runtime.remove_near_heap_limit_callback(config.memory_limit_bytes);
            let output = execute_python(
                &mut runtime,
                RuntimeRequest {
                    code: PYTHON_HEAP_PRESSURE.into(),
                    files: vec![],
                    timeout_ms: 120_000,
                },
                Arc::new(AtomicBool::new(false)),
            )
            .await;
            panic!("unguarded Python allocation did not abort: {output:?}");
        }

        // Both on-demand creation and warm replacement must remain usable.
        for prewarmed in [0, 1] {
            let config: AppConfig = toml::from_str(&format!(
                "[workers]\nmax_workers=1\nprewarmed_workers={prewarmed}\nmemory_limit_mb=512"
            ))
            .unwrap();
            let runtime = PyodideRuntime::new(&config).await.unwrap();
            let output = run(&runtime, PYTHON_HEAP_PRESSURE).await;
            assert!(!output.success, "{output:?}");
            let error = output.error.as_ref().expect("memory-limit error");
            assert_eq!(error.kind, "memory_limit", "{output:?}");
            assert!(error.message.contains("512 MiB"), "{output:?}");
            assert!(!output.timed_out, "{output:?}");

            let output = run(&runtime, "assert 'retained' not in globals()\nprint(6 * 7)").await;
            assert!(output.success, "{output:?}");
            assert_eq!(output.stdout, "42\n");
            assert!(runtime.health().is_ready());
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn runtime_applies_heap_limit_to_fresh_isolates() {
        for source in ["", "[workers]\nmemory_limit_mb = 1024"] {
            let config: AppConfig = toml::from_str(source).unwrap();
            let runtime_config = RuntimeConfig {
                memory_limit_bytes: config.workers.memory_limit_bytes().unwrap(),
                allowed_hosts: Arc::new(HashSet::new()),
                asset_dir: Arc::new(PathBuf::new()),
                client: reqwest::Client::new(),
                download_cache: Arc::new(Mutex::new(HashMap::new())),
                content_types: Arc::new(Mutex::new(HashMap::new())),
                preinstall_packages: Vec::new(),
                runtime_package_installation: true,
                package_cache_dir: Arc::new(PathBuf::new()),
                package_layer: None,
            };
            // Startup, cold workers and warm replacements share this factory.
            for _ in 0..2 {
                let mut runtime = create_runtime(&runtime_config.clone()).unwrap();
                runtime
                    .execute_script(
                        "<test_no_input_buffers>",
                        r#"
                    let rejected = false;
                    try { __takeInputFile(0); } catch (_) { rejected = true; }
                    if (!rejected) throw Error('buffer leaked across isolates');
                "#,
                    )
                    .unwrap();
                runtime
                    .op_state()
                    .borrow_mut()
                    .put(InputFileBuffers(vec![Some(vec![0, 255]), Some(vec![])]));
                runtime.execute_script("<test_input_buffers>", r#"
                    const bytes = __takeInputFile(0);
                    if (bytes.length !== 2 || bytes[0] !== 0 || bytes[1] !== 255) throw Error('bytes');
                    if (__takeInputFile(1).length !== 0) throw Error('empty');
                    for (const index of [0, 1, 2, 4294967295]) {
                        let rejected = false;
                        try { __takeInputFile(index); } catch (_) { rejected = true; }
                        if (!rejected) throw Error('input buffer reused or out of bounds');
                    }
                "#).unwrap();
                assert_eq!(
                    runtime.v8_isolate().get_heap_statistics().heap_size_limit(),
                    runtime_config.memory_limit_bytes,
                );
            }
        }
    }

    fn request() -> RuntimeRequest {
        RuntimeRequest {
            code: "pass".into(),
            files: vec![],
            timeout_ms: 1000,
        }
    }

    #[test]
    fn queue_metrics_cover_dispatch_cancel_overload_shutdown_and_worker_loss() {
        let recorder = app_metrics::builder().unwrap().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            app_metrics::register(1);
            let (runtime, worker) = PyodideRuntime::test_pool();
            let (id, receiver) = runtime
                .submit(request(), Arc::new(AtomicBool::new(false)))
                .ok()
                .unwrap();
            assert!(
                runtime
                    .submit(request(), Arc::new(AtomicBool::new(false)))
                    .is_err()
            );
            runtime.cancel_queued(id);
            drop(receiver);
            let (_, receiver) = runtime
                .submit(request(), Arc::new(AtomicBool::new(false)))
                .ok()
                .unwrap();
            let job = runtime.pool.queue.next().unwrap();
            drop(job);
            drop(receiver);
            let (_, receiver) = runtime
                .submit(request(), Arc::new(AtomicBool::new(false)))
                .ok()
                .unwrap();
            // Last worker exits unexpectedly; its guard drains the queue.
            drop(worker);
            assert!(
                runtime
                    .submit(request(), Arc::new(AtomicBool::new(false)))
                    .is_err()
            );
            drop(receiver);
        });
        let output = handle.render();
        for sample in [
            "execution_queue_depth 0",
            "workers{state=\"idle\"} 0",
            "worker_unexpected_exits_total 1",
            "execution_rejections_total{reason=\"queue_full\"} 1",
            "execution_rejections_total{reason=\"shutdown\"} 1",
            "execution_queue_wait_seconds_count{outcome=\"dispatched\"} 1",
            "execution_queue_wait_seconds_count{outcome=\"cancelled\"} 1",
            "execution_queue_wait_seconds_count{outcome=\"shutdown\"} 1",
        ] {
            assert!(
                output.contains(&format!("code_interpreter_mcp_{sample}")),
                "{sample}: {output}"
            );
        }
    }

    #[test]
    fn probes_ignore_queue_lock_and_saturation_and_shutdown_cancels_queue() {
        let (runtime, worker) = PyodideRuntime::test_pool();
        let (_, mut active_response) = runtime
            .submit(request(), Arc::new(AtomicBool::new(false)))
            .ok()
            .unwrap();
        let active_job = runtime.pool.queue.next().unwrap();
        let (_, mut response) = runtime
            .submit(request(), Arc::new(AtomicBool::new(false)))
            .ok()
            .unwrap();
        assert!(
            runtime
                .submit(request(), Arc::new(AtomicBool::new(false)))
                .is_err()
        );
        {
            let _lock = runtime.pool.queue.state.lock().unwrap();
            assert!(runtime.health().is_live());
            assert!(runtime.health().is_ready());
        }
        runtime.shutdown();
        assert!(runtime.health().is_live());
        assert!(!runtime.health().is_ready());
        assert!(!response.try_recv().unwrap().success);
        assert!(matches!(
            active_response.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        active_job
            .response
            .send(RunPythonCodeOutput::runtime("individual script error"))
            .unwrap();
        assert!(!active_response.try_recv().unwrap().success);
        assert!(runtime.health().is_live());
        assert!(
            runtime
                .submit(request(), Arc::new(AtomicBool::new(false)))
                .is_err()
        );
        assert!(runtime.pool.queue.next().is_none());
        drop(worker);
        assert_eq!(runtime.health().live_workers(), 0);
        assert!(runtime.health().is_live());
    }

    #[test]
    fn worker_panic_tracks_partial_and_total_loss() {
        let (runtime, first) = PyodideRuntime::test_pool();
        let second = WorkerGuard::new(runtime.pool.queue.clone());
        assert!(
            std::thread::spawn(move || {
                let _guard = first;
                panic!("worker failed");
            })
            .join()
            .is_err()
        );
        assert_eq!(runtime.health().live_workers(), 1);
        assert!(runtime.health().is_ready());
        let (_, mut response) = runtime
            .submit(request(), Arc::new(AtomicBool::new(false)))
            .ok()
            .unwrap();
        assert!(
            std::thread::spawn(move || {
                let _guard = second;
                panic!("last worker failed");
            })
            .join()
            .is_err()
        );
        assert!(!runtime.health().is_live());
        assert!(!runtime.health().is_ready());
        assert!(!response.try_recv().unwrap().success);
    }

    #[test]
    fn poisoned_queue_is_fatal_even_with_surviving_workers() {
        let (runtime, first) = PyodideRuntime::test_pool();
        let second = WorkerGuard::new(runtime.pool.queue.clone());
        let queue = runtime.pool.queue.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = first;
                let _lock = queue.state.lock().unwrap();
                panic!("queue failure");
            })
            .join()
            .is_err()
        );
        assert!(!runtime.health().is_live());
        assert!(!runtime.health().is_ready());
        assert!(
            runtime
                .submit(request(), Arc::new(AtomicBool::new(false)))
                .is_err()
        );
        drop(second);
    }

    #[tokio::test]
    async fn missing_assets_fail_startup() {
        let mut config: AppConfig = toml::from_str("").unwrap();
        config.asset_dir = Some(PathBuf::from("/nonexistent-ermcp-assets"));
        assert!(PyodideRuntime::new(&config).await.is_err());
    }

    async fn run(runtime: &PyodideRuntime, code: &str) -> RunPythonCodeOutput {
        runtime
            .execute(RuntimeRequest {
                code: code.to_owned(),
                files: Vec::new(),
                timeout_ms: 120_000,
            })
            .await
    }

    #[tokio::test]
    #[ignore = "requires npm ci and HTTPS package downloads"]
    async fn input_buffers_mount_and_are_isolated() {
        let config: AppConfig = toml::from_str("").unwrap();
        let runtime = PyodideRuntime::new(&config).await.unwrap();
        let input = crate::RunPythonCodeRequest {
            code: r#"from pathlib import Path
assert Path('nested/binary.bin').read_bytes() == bytes(range(256))
assert Path('unicode.txt').read_text() == 'Grüße 🦀'
assert Path('empty').read_bytes() == b''
Path('nested/binary.bin').write_bytes(b'changed')
print('mounted')"#
                .into(),
            files: [
                ("nested/binary.bin", (0..=255).collect::<Vec<u8>>()),
                ("unicode.txt", "Grüße 🦀".as_bytes().to_vec()),
                ("empty", vec![]),
            ]
            .into_iter()
            .map(|(path, bytes)| crate::InputFile {
                path: path.into(),
                content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            })
            .collect(),
            timeout_ms: None,
        };
        let request =
            crate::validate_request(input, config.input_limits.validate().unwrap()).unwrap();
        let output = runtime.execute(request).await;
        assert!(output.success, "{output:?}");
        assert_eq!(output.stdout, "mounted\n");
        assert!(output.files.is_empty());
        let output = run(
            &runtime,
            "from pathlib import Path\nassert not list(Path('/workspace').iterdir())",
        )
        .await;
        assert!(output.success, "{output:?}");
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
