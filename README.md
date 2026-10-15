# code-interpreter-mcp

An MCP Streamable HTTP server exposing one tool, `run_python_code`. Python runs
in a fresh [Pyodide](https://pyodide.org/) WebAssembly instance embedded
directly in Rust through `deno_core`; no browser, Node.js process, Bun process,
or Deno executable is used at runtime.

The server supports MCP protocol versions through `2026-07-28`. Clients using
`2026-07-28` communicate through stateless requests without an initialize
handshake or session ID. Clients using earlier supported protocol versions
continue to use the Streamable HTTP session lifecycle.

## Architecture

```text
MCP client
    │ Streamable HTTP at /mcp
    ▼
Rust + rmcp
    │ validated code, limits, Base64 files
    ▼
fresh deno_core / V8 isolate
    │ minimal Web APIs + capability-checked Rust fetch/random ops
    ▼
Pyodide Wasm + Emscripten MEMFS (/workspace)
```

Each call gets a new isolate, Pyodide interpreter, and virtual filesystem.
The embedded JavaScript realm does not install Deno filesystem, process,
environment, FFI, network, or Node compatibility extensions. Package downloads
are routed through an HTTPS host allowlist in Rust. A watchdog uses V8's
thread-safe isolate handle to terminate JavaScript and WebAssembly execution
when the tool timeout expires.

## Build

Prerequisites:

- Rust 1.91 or newer
- npm for the setup step only; Node.js is not used to run the server

```bash
npm install --ignore-scripts
cargo build --release
```

`npm install` obtains the pinned Pyodide distribution assets. The runtime
binary is `target/release/code-interpreter-mcp`.

Start the server:

```bash
./target/release/code-interpreter-mcp
```

It listens on `127.0.0.1:8000` by default and announces the effective port and
endpoint after binding:

```text
code-interpreter-mcp listening on port 8000 at http://127.0.0.1:8000/mcp
```

Example MCP client configuration:

```json
{
  "mcpServers": {
    "code-interpreter-mcp": {
      "url": "http://127.0.0.1:8000/mcp"
    }
  }
}
```

## Tool input

```json
{
  "code": "from pathlib import Path\nprint(Path('data/input.txt').read_text())",
  "files": [
    {
      "path": "data/input.txt",
      "content_base64": "aGVsbG8K"
    }
  ],
  "timeout_ms": 30000
}
```

- `code` is required and supports top-level `await`.
- `files` is optional. Paths are relative to `/workspace`; absolute paths,
  parent traversal, duplicate paths, and backslashes are rejected.
- `timeout_ms` defaults to 30,000 and may not exceed 120,000.

The result contains `success`, captured `stdout` and `stderr`, the string form
of the final Python expression, `files`, a structured error/traceback,
`timed_out`, and `output_truncated`. `files` contains regular files that are
present below `/workspace` at the end of execution but were not present when
the script started. Each entry has `path`, `content_base64`, and a `mime_type`
inferred from the path. Input files and temporary files deleted before
execution ends are not returned.

For example, this creates `files` containing a Base64-encoded `results.csv`:

```python
import pandas as pd

frame = pd.DataFrame({"value": [1, 2, 3]})
frame.loc[frame["value"] > 1].to_csv("results.csv", index=False)
```

### pandas and Pyodide packages

With runtime package installation enabled (the default), imports recognized by
Pyodide are loaded automatically:

```python
import pandas as pd

frame = pd.DataFrame({"value": [1, 2, 3]})
frame["value"].sum()
```

### Package preinstallation and installation policy

```toml
preinstall_packages = ["pandas", "snowballstemmer==3.0.1"]
runtime_package_installation = false
```

`preinstall_packages` defaults to `[]`. It accepts micropip-compatible package
requirements (including version constraints and compatible wheel URLs). On
server startup, packages and their dependencies are installed into a durable
local package layer when the layer is missing or no longer matches the configured
requirements. Each fresh sandbox copies package files from that layer; Python
interpreter state is never shared between calls. Initial preparation happens
before the server accepts requests and is outside `timeout_ms`. Progress,
package names, completion, and diagnostic failures are logged to server stderr.
Preparation failures prevent the server from starting. Later config files
replace the entire package list.

The `run_python_code` description returned by `tools/list` lists configured
pre-installed libraries by package name, omitting version constraints and wheel
URL details. It advertises `micropip.install()` only when runtime package
installation is enabled.

`runtime_package_installation` defaults to `true`. When enabled, recognized
imports and explicit `micropip.install` calls retain their normal behavior. When
`false`, automatic package loading is skipped; the standard library and packages
installed during initialization (including dependencies) remain available. An
unavailable import fails with a Python import error. `micropip.install` raises
`RuntimeError: Runtime package installation is disabled`, even after reimporting
or reloading micropip. Pyodide package-loading APIs are also disabled.

In disabled mode, sandbox HTTPS downloads are blocked after initialization,
including cached downloads, to prevent alternate download-based installers.
Preinstallation still uses the configured HTTPS host allowlist. This is a package
installation policy, not a restriction on executing Python source supplied in
the request or its input files.

### Configured code snippets

The server can expose configured Python snippets through `list_code_snippets`
and `get_code_snippet`. Listing returns snippet IDs and descriptive metadata;
search is case-insensitive and checks titles and descriptions. Query words are
split on punctuation and whitespace, deduplicated, and matched as substrings.
Results matching more query words rank first, with title matches breaking ties,
then snippet ID for stable ordering. Words can match across fields; unmatched
words do not disqualify a snippet. For example, `DOCX create document` can find a
text document conversion snippet, and `XLSX risk register` can find an Excel
workbook snippet even if its description only mentions XLSX.

All matching snippets are returned. `[search] min_hits = 3` (the default) fills
short result lists with the next best available snippets, up to the size of the
visible catalog. This is a minimum, not a result limit. Set `min_hits = 0` to
return only matches, including an empty list for an unrelated query. Fallback
results can be unrelated: inspect their descriptions before using them. Include
and exclude filters still apply before searching. An omitted, blank, or
punctuation-only query lists all visible snippets sorted by ID.
Getting a snippet by ID returns its full metadata and source code.

```toml
[search]
min_hits = 3

[code_snippets]
enabled = true
include_snippet_ids = []
include_snippet_tags = []
exclude_snippet_ids = []

[code_snippets.snippets.convert_docx_to_pdf]
title = "Convert DOCX to PDF"
description = "Takes a DOCX file as input and converts it to a PDF"
required_python_packages = ["magical_docx_to_pdf"]
tags = ["provider_eratolabs"]
code_snippet = '''
import magical_docx_to_pdf
# Convert the input document.
'''
```

Both snippet tools are enabled by default and can be hidden with
`enabled = false`. If either include list is non-empty, a snippet is included
when its ID is listed or it has at least one listed tag. Excluded IDs take
precedence. `required_python_packages` and `tags` default to empty lists.

### micropip

`micropip` is preloaded, so compatible pure-Python or Wasm wheels may be
installed from inside the submitted script:

```python
import micropip

await micropip.install("snowballstemmer")
import snowballstemmer
snowballstemmer.stemmer("english").stemWords(["running"])
```

`micropip` cannot install arbitrary native CPython wheels. Packages with native
code need a Pyodide-compatible wheel or a package already built in the Pyodide
repository.

## Configuration and limits

### Health probes and shutdown

The existing HTTP listener serves ordinary `GET /livez` and `GET /readyz`
requests without MCP initialization or session headers. Successful probes return
HTTP **200** with `ok`; unhealthy probes return **503** with `unavailable`.
Probe routes accept Pod-IP Host headers. Host verification remains enforced on
`/mcp` according to the settings below.

- `/livez` succeeds while HTTP responds and no unrecoverable worker-pool failure
  is known. Losing all workers unexpectedly, or poisoning the internal worker
  queue, fails liveness.
- `/readyz` additionally requires the pool to accept submissions and have at
  least one live worker. Busy workers count as alive. Partial worker loss leaves
  the pool ready while surviving workers remain operational; workers are not
  automatically restarted.

Probes only read shared atomics. They do not lock the execution queue, run Python,
enqueue work, or access package registries. Queue saturation, individual script
errors, and package download failures do not fail probes. A full queue continues
to return the existing structured overload error.

The listener is bound only after asset checks, configured package preparation,
and worker startup acknowledgements complete. Connection failures during startup
are expected; failed initialization does not expose a ready server. Configure
`workers.prewarmed_workers >= 1` for readiness to imply that Python initialized
successfully. With `prewarmed_workers = 0`, startup confirms worker thread creation
and asset presence, but does not initialize Pyodide. These probes do not detect
live workers stuck inside initialization.

SIGTERM (on Unix) or Ctrl-C starts a **30-second total shutdown deadline**.
Readiness immediately fails and new execution submissions are rejected with a
structured runtime error. Queued executions are cancelled immediately with a
runtime error; active executions may finish within the deadline. Liveness stays
successful during normal draining, and the listener continues serving probes
until workers exit. HTTP then shuts down gracefully. The same deadline bounds
persistent MCP streams and remaining active work; at the deadline the process
exits even if workers or streams have not finished.

### Queue and workers

The HTTP server's Tokio runtime defaults to the detected available parallelism,
with a minimum of four threads even in single-core containers. Set
`TOKIO_WORKER_THREADS` to a positive integer to override this count; values below
four are honored with a startup warning. Invalid values prevent startup. These
threads are separate from the Python execution workers configured below.

Code execution is handled by a shared FIFO queue and worker pool. Defaults are
two workers, eight queued requests, and one prewarmed worker. When the queue is
full, the tool returns a structured overload error. MCP clients that provide a
progress token receive a queue position after one second of waiting, then every
five seconds while the request remains queued. Queue wait does not consume
`timeout_ms`.

```toml
package_cache_dir = ".code-interpreter-mcp-package-cache"

[workers]
max_workers = 2
queue_capacity = 8
prewarmed_workers = 1
memory_limit_mb = 512
```

`workers.memory_limit_mb` sets the V8 heap budget for each isolate, including
startup and replacement isolates. It defaults to `512` and uses 1 MB = 1,048,576
bytes (512 MiB by default). Values must be positive integers whose byte conversion
fits the platform's address size; invalid values prevent startup. This is a
per-isolate V8 heap budget, not a total process memory limit. Size the container
for `workers.max_workers` concurrent heaps plus Pyodide/WebAssembly memory, input
buffers, caches, and other process overhead. Increasing the container memory
limit alone does not increase this heap budget.

Prewarmed workers initialize V8, Pyodide, and configured preinstalled packages
before accepting work. Each execution still gets a fresh isolate. The worker
discards that isolate after the call and prepares a clean replacement.

The server prepares a durable local package layer at startup when
`preinstall_packages` is non-empty. It reuses the layer when its Pyodide version
and package list match; otherwise it installs the configured packages and
rebuilds the layer. New isolates copy package files from this local layer.
The pinned Pyodide runtime files are served locally. Wheel files are not
included in the npm package, so the Docker build downloads and verifies the
micropip wheel against `pyodide-lock.json`. Other missing Pyodide package wheels
are fetched from the matching versioned jsDelivr distribution and must be
permitted by `allowed_hosts`. Runtime installs requested by user code still
follow `runtime_package_installation` and the configured HTTPS host allowlist.

The cache directory defaults to `.code-interpreter-mcp-package-cache` in the
working directory. Set `package_cache_dir` or
`CODE_INTERPRETER_MCP_PACKAGE_CACHE_DIR` to place it elsewhere. Worker settings
can be overridden with `CODE_INTERPRETER_MCP_MAX_WORKERS`,
`CODE_INTERPRETER_MCP_QUEUE_CAPACITY`, and
`CODE_INTERPRETER_MCP_PREWARMED_WORKERS`.

The server reads `code-interpreter-mcp.toml` and every
`*.auto.code-interpreter-mcp.toml` file in its current working directory.
Matching files are loaded in filename order. Values in later files override
earlier values; TOML tables are merged by key and arrays are replaced.
To use a specific ordered list instead, pass `--config path/to/file.toml` more
than once. An explicit list disables discovery, and every listed file must exist.

For example, `code-interpreter-mcp.toml` can contain:

```toml
bind_addr = "127.0.0.1:8000"
allowed_hosts = ["cdn.jsdelivr.net", "pypi.org", "files.pythonhosted.org"]
asset_dir = "/app/node_modules/pyodide"
```

All settings are optional. The existing environment variables override values
from configuration files:

- `CODE_INTERPRETER_MCP_BIND_ADDR`: HTTP socket address. Defaults to `127.0.0.1:8000`.
  Use `127.0.0.1:0` to let the operating system select a free port; the
  selected port is included in the startup announcement.
- `CODE_INTERPRETER_MCP_ALLOWED_HOSTS`: comma-separated HTTPS package hosts. Defaults to
  `cdn.jsdelivr.net,pypi.org,files.pythonhosted.org`. Subdomains are accepted.
- `CODE_INTERPRETER_MCP_ASSET_DIR`: overrides the local Pyodide asset directory. It must
  contain the files installed by the pinned `pyodide` npm package.
- `CODE_INTERPRETER_MCP_PACKAGE_CACHE_DIR`: overrides the durable local package layer directory.
- `CODE_INTERPRETER_MCP_MAX_WORKERS`: maximum simultaneous execution workers (default `2`).
- `CODE_INTERPRETER_MCP_QUEUE_CAPACITY`: maximum number of waiting requests (default `8`).
- `CODE_INTERPRETER_MCP_PREWARMED_WORKERS`: worker isolates initialized before the server accepts requests (default `1`, maximum `workers.max_workers`).
- `CODE_INTERPRETER_MCP_HTTP_ALLOWED_HOSTS`: comma-separated inbound HTTP Host
  allowlist, overriding `http_allowed_hosts`. Whitespace and empty entries are ignored.
- `CODE_INTERPRETER_MCP_SKIP_HOST_HEADER_VERIFICATION`: `true` or `false`, overriding
  `skip_host_header_verification` (default `false`).
- `CODE_INTERPRETER_MCP_MAX_FILE_KB`: decoded KB allowed per input file,
  overriding `input_limits.max_file_kb` (default `10240`, 10 MiB).
- `CODE_INTERPRETER_MCP_MAX_TOTAL_FILE_KB`: decoded KB allowed across input
  files in one request, overriding `input_limits.max_total_file_kb`
  (default `51200`, 50 MiB). Both variables use 1 KB = 1,024 bytes.

### Input file and HTTP payload sizes

Set input limits in integer **KB**, measured after Base64 decoding. These
settings use **1 KB = 1,024 bytes** (1,024 KB = 1 MiB). Both settings must be
between 1 and 1,048,576 KB (1 GiB), and `max_file_kb` must not exceed
`max_total_file_kb`. Invalid settings prevent startup. Later configuration
files merge these settings by key; environment variables override them.

```toml
[input_limits]
max_file_kb = 51200        # 50 MiB per decoded input file
max_total_file_kb = 102400 # 100 MiB across all decoded input files
```

This example permits the approximately 40 MiB CSV from ERMCP-34 and permits
multiple files when each fits the per-file limit and their combined decoded
size fits the aggregate limit. The maximum file count remains 64.

The HTTP POST body ceiling is derived automatically from the aggregate limit;
there is no separate HTTP setting to synchronize. For aggregate decoded byte
allowance `T = max_total_file_kb * 1024`,
the ceiling is `4 * ceil(T / 3) + 4 * (64 - 1) + 8388608` bytes. This accounts
for Base64 expansion, independent padding of up to 64 files, and 8 MiB of
JSON/RPC headroom. The default 50 MiB aggregate produces a **78,293,928-byte**
HTTP ceiling (approximately 74.67 MiB); the 100 MiB example produces a
**148,198,996-byte** ceiling (approximately 141.33 MiB).

Headroom covers JSON/RPC fields, file paths, and Python source (including up to
6 MiB for JSON escaping of the permitted 1 MiB source). Send canonical Base64
strings directly in JSON. Excessive whitespace, unusually large paths or
metadata, or escaping the Base64 characters themselves consumes headroom and
can reach the HTTP ceiling even when decoded files fit. The whole HTTP body
remains bounded.

Input validation returns `error.kind = "validation"` before queueing or code
execution. Its message identifies either the per-file or aggregate limit,
the relevant configuration key, and the byte allowance. HTTP bodies over the
derived ceiling receive **HTTP 413**, with a plain-text message identifying
the HTTP limit and its aggregate setting. HTTP rejections and effective size
limits are logged to server stderr so operators can diagnose clients that
report only a generic transport failure. A reverse proxy may impose its own
body limit and must also allow the derived ceiling.

### HTTP Host header verification

Streamable HTTP verifies the inbound `Host` header by default, allowing only
`localhost`, `127.0.0.1`, and `::1` on any port. For a public hostname or reverse
proxy, configure the hosts that clients or the proxy send to this server:

```toml
http_allowed_hosts = ["mcp.example.com", "proxy.example.com:8443"]
skip_host_header_verification = false
```

A non-empty `http_allowed_hosts` list replaces the default loopback allowlist.
A hostname permits any port; a `host:port` entry permits only that port. Omitted
or empty lists retain the loopback defaults. These settings are independent of
`bind_addr` and the outbound HTTPS package download `allowed_hosts` setting.

Set `skip_host_header_verification = true` to allow any inbound Host value,
even when `http_allowed_hosts` is configured. This disables the Host allowlist
protection against DNS rebinding and is not recommended for public deployments.

The committed [configuration key reference](generated/config-reference.json)
is generated from `AppConfig` with `facet`. After changing its fields, run
`cargo run --bin gen-config-reference` and commit the updated JSON. CI runs
the generator with `--check`.

Per request limits:

- Python source: 1 MiB
- files: 64
- one decoded input file: configurable, default 10 MiB
- all decoded input files: configurable, default 50 MiB
- HTTP request body: derived from input allowance, default 78,293,928 bytes
- one network response: 100 MiB
- stdout and stderr: 512 KiB each
- final expression: 64 KiB
- returned files: 64
- one returned file: 10 MiB
- all returned files: 50 MiB

If a returned-file limit is exceeded, whole files outside the limit are
omitted and `output_truncated` is set to `true`.

Downloaded package artifacts are cached in memory for the life of the server.
Configured preinstalled packages are also captured in a durable local package
layer at server startup and copied into fresh isolates. The package layer is
recreated when the Pyodide version or configured package list changes.

## Security scope

The implementation deliberately exposes only MEMFS, allowlisted HTTPS fetch,
and OS randomness to Pyodide. It also removes `Deno`, `process`, `Bun`,
`require`, Pyodide native filesystem mounts, and Deno's internal bootstrap from
the reachable JavaScript global.

WebAssembly and a capability-minimal V8 embed are strong isolation layers for
ordinary untrusted Python, but they are not a substitute for an OS security
boundary against a V8/Pyodide engine vulnerability or process-wide memory
exhaustion. For adversarial multi-tenant use, run this MCP server inside a
container or another OS sandbox with memory, CPU, filesystem, and network
limits.

## Development

```bash
cargo fmt --check
cargo test
# Integration coverage requires npm assets and HTTPS package access:
cargo test package_policy_in_fresh_sandboxes -- --ignored --nocapture
cargo clippy --all-targets --all-features -- -D warnings
```

## License

This project is licensed under the [MIT License](LICENSE).

Third-party dependencies retain their respective licenses.

## Prometheus metrics

Metrics are runtime opt-in; no rebuild is needed. Defaults (also used for omitted
keys in a partial section):

```toml
[integrations.prometheus]
enabled = false
host = "127.0.0.1"
port = 3131
```

To allow scraping from outside a container:

```toml
[integrations.prometheus]
enabled = true
host = "0.0.0.0"
port = 3131
```

| TOML key | Default | Environment override |
| --- | --- | --- |
| `integrations.prometheus.enabled` | `false` | `CODE_INTERPRETER_MCP_PROMETHEUS_ENABLED` (`true`/`false`) |
| `integrations.prometheus.host` | `"127.0.0.1"` | `CODE_INTERPRETER_MCP_PROMETHEUS_HOST` |
| `integrations.prometheus.port` | `3131` | `CODE_INTERPRETER_MCP_PROMETHEUS_PORT` |

Environment overrides apply after TOML merging. When enabled, the host must be
an IPv4 or IPv6 address (e.g. `::1`), the port must be 1–65535 and must differ from
`bind_addr`'s port, even when the addresses differ. Invalid settings, bind failures,
and recorder-installation failures stop startup with an error.

Scrape `http://127.0.0.1:3131/metrics` (or `http://[::1]:3131/metrics` for IPv6).
The fixed `/metrics` route uses a dedicated listener, independent of MCP Host
verification and sessions, with no authentication. Expose its port only to intended
scrapers using your deployment's network controls. The listener starts before Python
initialization: **metrics availability does not imply readiness**. Continue using
`/readyz` for readiness and `/livez` for liveness. Disabled metrics install no
recorder, open no listener, and spawn no reporter. Scrapes read recorded samples
without acquiring the execution queue lock or running Python/network fetches.

All ten application metric families below have the `code_interpreter_mcp_` prefix.
Counters and gauges start at zero (queue capacity uses the configured value);
histograms are registered without synthetic observations. Labels never contain code,
paths, package names, URLs, session IDs, or error messages.

| Metric suffix | Type | Labels and meaning |
| --- | --- | --- |
| `tool_calls_total` | Counter | `tool`, `outcome`: each completed or cancelled handler once |
| `tool_call_duration_seconds` | Histogram | `tool`, `outcome`: handler entry to completion, including queue/init |
| `execution_queue_depth` | Gauge | Jobs waiting for a worker |
| `execution_queue_capacity` | Gauge | Configured maximum waiting jobs |
| `execution_queue_wait_seconds` | Histogram | `outcome`: `dispatched`, `cancelled`, `shutdown`; enqueue until removal |
| `workers` | Gauge | `state`: `idle`, `initializing`, `executing`; mutually exclusive live worker states |
| `python_execution_duration_seconds` | Histogram | `outcome`: `success`, `python_error`, `timeout`, `runtime_failure`; excludes queue/init |
| `isolate_initialization_duration_seconds` | Histogram | `phase`: `startup`, `on_demand`, `replenishment`; `outcome`: `success`, `runtime_failure` |
| `execution_rejections_total` | Counter | `reason`: `validation`, `queue_full`, `shutdown` |
| `worker_unexpected_exits_total` | Counter | Worker exits outside normal shutdown, including panics |

Tool labels are `run_python_code`, `list_code_snippets`, `get_code_snippet`.
Tool outcomes are `success`, `validation_error`, `overload`, `python_error`,
`timeout`, `cancelled`, `runtime_failure`. Structured tool results determine
outcomes, not HTTP status. These metrics cover handlers entered after MCP argument
parsing; malformed protocol requests rejected before handler entry are not tool calls.
A missing snippet is a successful lookup returning null. Rejections intentionally
count a subset of tool calls separately. Cancellation removes queued jobs, but
already-running Python may finish after its caller has gone; execution observations
remain separate. Shutdown removes queued jobs and lets active workers drain.

All durations use `Duration::as_secs_f64()` in seconds (25 ms = 0.025).
Python execution buckets are `0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5,
1, 2.5, 5, 10, 30, 60, 120`. Queue wait, isolate initialization, and handler
histograms use those boundaries plus `300, 600`. All include the implicit `+Inf`
bucket. Buckets control histogram resolution, not recorded duration precision.

The same switch enables a five-second `tokio-metrics` reporter for the **main Tokio
runtime**, not the dedicated Python worker threads (covered by `workers` and the
execution/init metrics). Stable-runtime gauges are `tokio_workers_count`,
`tokio_live_tasks_count`, `tokio_global_queue_depth`, `tokio_total_park_count`,
`tokio_min_park_count`, `tokio_max_park_count`, `tokio_total_busy_duration_seconds`,
`tokio_min_busy_duration_seconds`, `tokio_max_busy_duration_seconds`, and
`tokio_busy_ratio`. Park counts and busy durations describe the last reporting
interval; busy ratio is the fraction of available worker time spent busy. The
monitor's durations are converted to fractional seconds rather than using its
built-in reporter's integer microseconds. No unstable Tokio compiler flags,
per-metric toggles, or configurable histogram buckets are required.
