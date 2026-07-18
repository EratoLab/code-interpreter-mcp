(async () => {
  const safeGlobals = globalThis.__createSafeGlobals();
  const pyodide = await globalThis.loadPyodide({
    indexURL: "https://pyodide.local/",
    lockFileURL: "https://pyodide.local/pyodide-lock.json",
    packageBaseUrl: "https://cdn.jsdelivr.net/pyodide/v314.0.2/full/",
    createPyodideModule: globalThis.createPyodideModule,
    jsglobals: safeGlobals,
    stdout: (line) => globalThis.__captureLine("stdout", line),
    stderr: (line) => globalThis.__captureLine("stderr", line),
  });

  await pyodide.loadPackage("micropip");
  if (!pyodide.loadedPackages.micropip) {
    throw new Error(
      `micropip did not load: ${JSON.stringify(globalThis.__capture)}`,
    );
  }

  // Browser filesystem adapters are unnecessary and would widen the visible
  // API. Pyodide's in-memory Emscripten filesystem remains available.
  for (const property of ["mountNativeFS", "mountNodeFS"]) {
    try {
      Object.defineProperty(pyodide, property, {
        value: undefined,
        configurable: false,
        writable: false,
      });
    } catch {
      try {
        pyodide[property] = undefined;
      } catch {}
    }
  }

  globalThis.__pyodide = pyodide;
  globalThis.createPyodideModule = undefined;
  globalThis.__resetCapture();
})()
