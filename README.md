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

Imports recognized by Pyodide are loaded automatically:

```python
import pandas as pd

frame = pd.DataFrame({"value": [1, 2, 3]})
frame["value"].sum()
```

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
cargo clippy --all-targets --all-features -- -D warnings
```
