"use strict";

{
  const core = globalThis.Deno.core;
  const { op_fetch_content_type, op_fetch_url, op_random_fill } = core.ops;
  const { URL, URLSearchParams } = core.loadExtScript("ext:deno_web/00_url.js");
  const { TextDecoder, TextEncoder } = core.loadExtScript(
    "ext:deno_web/08_text_encoding.js",
  );
  const { performance } = core.loadExtScript(
    "ext:deno_web/15_performance.js",
  );
  const { AbortController, AbortSignal } = core.loadExtScript(
    "ext:deno_web/03_abort_signal.js",
  );
  const { EventTarget } = core.loadExtScript("ext:deno_web/02_event.js");
  const { setTimeout, clearTimeout, setInterval, clearInterval } =
    core.loadExtScript("ext:deno_web/02_timers.js");
  const { atob, btoa } = core.loadExtScript("ext:deno_web/05_base64.js");

  const OUTPUT_LIMIT_BYTES = 512 * 1024;
  const RESULT_LIMIT_BYTES = 64 * 1024;
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();

  class SandboxHeaders {
    #values = new Map();

    constructor(values = []) {
      const entries = values instanceof SandboxHeaders
        ? values.entries()
        : typeof values?.[Symbol.iterator] === "function"
          ? values
          : Object.entries(values);
      for (const [key, value] of entries) {
        this.#values.set(String(key).toLowerCase(), String(value));
      }
    }

    get(name) {
      return this.#values.get(String(name).toLowerCase()) ?? null;
    }

    entries() {
      return this.#values.entries();
    }

    [Symbol.iterator]() {
      return this.entries();
    }
  }

  class SandboxRequest {
    constructor(input, init = {}) {
      this.url = String(input instanceof SandboxRequest ? input.url : input);
      this.method = String(init.method ?? "GET").toUpperCase();
      this.headers = new SandboxHeaders(init.headers ?? []);
      this.signal = init.signal ?? null;
    }
  }

  class SandboxResponse {
    #bytes;

    constructor(bytes, url, contentType = "application/octet-stream") {
      this.#bytes = new Uint8Array(bytes);
      this.url = url;
      this.status = 200;
      this.statusText = "OK";
      this.ok = true;
      this.redirected = false;
      this.type = "basic";
      this.bodyUsed = false;
      this.headers = new SandboxHeaders([["content-type", contentType]]);
    }

    #takeBytes() {
      if (this.bodyUsed) throw new TypeError("Response body has already been used");
      this.bodyUsed = true;
      return this.#bytes;
    }

    async arrayBuffer() {
      const bytes = this.#takeBytes();
      return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    }

    async text() {
      return decoder.decode(this.#takeBytes());
    }

    async json() {
      return JSON.parse(await this.text());
    }

    clone() {
      if (this.bodyUsed) throw new TypeError("Response body has already been used");
      return new SandboxResponse(
        this.#bytes.slice(),
        this.url,
        this.headers.get("content-type"),
      );
    }
  }

  async function sandboxFetch(input, init = {}) {
    const request = input instanceof SandboxRequest
      ? input
      : new SandboxRequest(input, init);
    if (request.method !== "GET") {
      throw new TypeError("Only GET requests are supported in the Python sandbox");
    }
    if (request.signal?.aborted) {
      throw request.signal.reason ?? new Error("The request was aborted");
    }
    const bytes = await op_fetch_url(request.url);
    if (request.signal?.aborted) {
      throw request.signal.reason ?? new Error("The request was aborted");
    }
    return new SandboxResponse(
      bytes,
      request.url,
      op_fetch_content_type(request.url),
    );
  }

  const crypto = Object.freeze({
    getRandomValues(view) {
      if (!ArrayBuffer.isView(view) || view instanceof DataView) {
        throw new TypeError("getRandomValues expects an integer TypedArray");
      }
      if (view.byteLength > 65_536) {
        throw new TypeError("getRandomValues is limited to 65536 bytes");
      }
      op_random_fill(
        new Uint8Array(view.buffer, view.byteOffset, view.byteLength),
      );
      return view;
    },
  });

  function resetCapture() {
    globalThis.__capture = {
      stdout: "",
      stderr: "",
      stdoutBytes: 0,
      stderrBytes: 0,
      truncated: false,
    };
  }

  function captureLine(stream, value) {
    const capture = globalThis.__capture;
    const sizeKey = `${stream}Bytes`;
    const bytes = encoder.encode(`${String(value)}\n`);
    const remaining = OUTPUT_LIMIT_BYTES - capture[sizeKey];
    if (bytes.byteLength > remaining) capture.truncated = true;
    if (remaining <= 0) return;
    const chunk = bytes.subarray(0, remaining);
    capture[stream] += decoder.decode(chunk);
    capture[sizeKey] += chunk.byteLength;
  }

  const console = Object.freeze({
    log: (...values) => captureLine("stdout", values.map(String).join(" ")),
    info: (...values) => captureLine("stdout", values.map(String).join(" ")),
    debug: (...values) => captureLine("stdout", values.map(String).join(" ")),
    warn: (...values) => captureLine("stderr", values.map(String).join(" ")),
    error: (...values) => captureLine("stderr", values.map(String).join(" ")),
  });
  const globalEvents = new EventTarget();

  function createSafeGlobals() {
    const globals = Object.create(null);
    Object.assign(globals, {
      AbortController,
      AbortSignal,
      Headers: SandboxHeaders,
      Object,
      Request: SandboxRequest,
      Response: SandboxResponse,
      TextDecoder,
      TextEncoder,
      URL,
      URLSearchParams,
      fetch: sandboxFetch,
    });
    return globals;
  }

  Object.assign(globalThis, {
    AbortController,
    AbortSignal,
    Headers: SandboxHeaders,
    Request: SandboxRequest,
    Response: SandboxResponse,
    TextDecoder,
    TextEncoder,
    URL,
    URLSearchParams,
    atob,
    addEventListener: globalEvents.addEventListener.bind(globalEvents),
    btoa,
    clearInterval,
    clearTimeout,
    console,
    crypto,
    fetch: sandboxFetch,
    performance,
    removeEventListener: globalEvents.removeEventListener.bind(globalEvents),
    dispatchEvent: globalEvents.dispatchEvent.bind(globalEvents),
    setInterval,
    setTimeout,
  });

  globalThis.location = new URL("https://pyodide.local/");
  globalThis.document = Object.freeze({ createElement: () => ({}) });
  globalThis.sessionStorage = Object.freeze({});
  globalThis.navigator = Object.freeze({
    maxTouchPoints: 0,
    platform: "deno_core",
    userAgent: "code-interpreter-mcp-deno-core",
  });
  globalThis.window = globalThis;
  globalThis.self = globalThis;
  globalThis.__createSafeGlobals = createSafeGlobals;
  globalThis.__resetCapture = resetCapture;
  globalThis.__captureLine = captureLine;
  globalThis.__resultLimitBytes = RESULT_LIMIT_BYTES;
  resetCapture();

  // V8's native implementation expects a native Fetch Response. The sandbox
  // response intentionally has a much smaller surface, so instantiate through
  // its ArrayBuffer instead.
  WebAssembly.instantiateStreaming = async (response, imports) => {
    const resolved = await response;
    return WebAssembly.instantiate(await resolved.arrayBuffer(), imports);
  };

  // Retain only the two capability-checked op closures above. Removing Deno
  // prevents Python/JavaScript from discovering arbitrary host operations.
  try {
    delete globalThis.Deno;
  } catch {
    globalThis.Deno = undefined;
  }
  try {
    delete globalThis.__bootstrap;
  } catch {
    globalThis.__bootstrap = undefined;
  }
}
