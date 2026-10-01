# code-interpreter-mcp

An MCP Streamable HTTP server exposing one tool, `run_python_code`. Python runs
in a fresh [Pyodide](https://pyodide.org/) WebAssembly instance embedded
directly in Rust through `deno_core`; no browser, Node.js process, Bun process,
or Deno executable is used at runtime.

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
search is case-insensitive and checks titles and descriptions. Getting a snippet
by ID returns its full metadata and source code. Snippets are sorted by ID.

```toml
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

### Queue and workers

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
```

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
- one decoded file: 10 MiB
- all decoded files: 50 MiB
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
