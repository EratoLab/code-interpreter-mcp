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

  const policy = globalThis.__packagePolicy;
  const log = globalThis.__packageLog;
  const packages = policy.preinstall_packages;
  log(`initializing fresh sandbox; preinstall packages: ${JSON.stringify(packages)}`);
  try {
    await pyodide.loadPackage("micropip", {
      messageCallback: (message) => log(message),
      errorCallback: (message) => log(`ERROR: ${message}`),
    });
    if (!pyodide.loadedPackages.micropip) {
      throw new Error(`micropip did not load: ${JSON.stringify(globalThis.__capture)}`);
    }

    const micropip = pyodide.pyimport("micropip");
    try {
      // Resolve the complete list together so version constraints are respected.
      log(`installing configured packages and dependencies: ${JSON.stringify(packages)}`);
      await micropip.install(packages);
      const installed = pyodide.runPython("str(__import__('micropip').list())");
      log(`preinstallation complete: ${JSON.stringify(packages)}; installed packages: ${installed}`);
    } finally {
      micropip.destroy();
    }

    if (!policy.runtime_package_installation) {
      // Replace the on-disk installer as well as loaded modules: importing or
      // reloading micropip must never restore the original installation API.
      pyodide.runPython(`
import micropip as _micropip
import pathlib as _pathlib
import shutil as _shutil
import sys as _sys
_micropip_dir = _pathlib.Path(_micropip.__file__).parent
_shutil.rmtree(_micropip_dir)
_micropip_dir.mkdir()
(_micropip_dir / "__init__.py").write_text(
    'async def install(*args, **kwargs):\\n'
    '    raise RuntimeError("Runtime package installation is disabled")\\n'
)
for _name in list(_sys.modules):
    if _name == "micropip" or _name.startswith("micropip."):
        del _sys.modules[_name]
del _micropip, _pathlib, _shutil, _sys, _micropip_dir, _name
`);
      const disabled = () => {
        throw new Error("Runtime package installation is disabled");
      };
      for (const name of ["loadPackage", "loadPackagesFromImports"]) {
        Object.defineProperty(pyodide, name, {
          value: disabled,
          configurable: false,
          writable: false,
        });
      }
    }
  } catch (error) {
    log(`ERROR: sandbox preinstallation failed for ${JSON.stringify(packages)}: ${error?.stack ?? error}`);
    throw new Error(`Sandbox package initialization failed for ${JSON.stringify(packages)}: ${error}`);
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
