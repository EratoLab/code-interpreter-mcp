globalThis.__pyodideInitialization = (async () => {
  const safeGlobals = globalThis.__createSafeGlobals();
  const pyodide = await globalThis.loadPyodide({
    indexURL: "https://pyodide.local/",
    lockFileURL: "https://pyodide.local/pyodide-lock.json",
    packageBaseUrl: "https://pyodide.local/",
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

    if (policy.package_layer) {
      const layer = policy.package_layer;
      const purelib = pyodide.runPython(
        "import sysconfig; sysconfig.get_path('purelib')",
      );
      if (purelib !== layer.purelib) {
        throw new Error(`Package cache Python path mismatch: ${purelib} != ${layer.purelib}`);
      }
      const pyodidePackages = layer.pyodide_packages.filter(
        (name) =>
          name !== "micropip" && Object.hasOwn(pyodide.lockfile.packages, name),
      );
      if (pyodidePackages.length > 0) {
        await pyodide.loadPackage(pyodidePackages, {
          messageCallback: (message) => log(message),
          errorCallback: (message) => log(`ERROR: ${message}`),
        });
      }
      await restorePackageFiles(pyodide.FS, purelib, layer.files);
      log(`copied ${layer.files.length} cached package files into the sandbox`);
    } else {
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
  if (policy.build_package_layer) {
    return JSON.stringify(await exportPackageLayer(pyodide, packages, policy.pyodide_version));
  }
})();

async function restorePackageFiles(FS, purelib, files) {
  for (const relativePath of files) {
    const target = `${purelib}/${relativePath}`;
    const parent = target.slice(0, target.lastIndexOf("/"));
    FS.mkdirTree(parent);
    const bytes = await globalThis.__readPackageLayerFile(relativePath);
    FS.writeFile(target, bytes);
  }
}

function encodeBase64(bytes) {
  const chunkSize = 32 * 1024;
  let binary = "";
  for (let offset = 0; offset < bytes.byteLength; offset += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunkSize));
  }
  return btoa(binary);
}

async function exportPackageLayer(pyodide, preinstallPackages, pyodideVersion) {
  const purelib = pyodide.runPython("import sysconfig; sysconfig.get_path('purelib')");
  const files = [];
  const directories = [purelib];
  while (directories.length > 0) {
    const directory = directories.pop();
    for (const name of pyodide.FS.readdir(directory)) {
      if (name === "." || name === "..") continue;
      const path = `${directory}/${name}`;
      const stat = pyodide.FS.lstat(path);
      if (pyodide.FS.isDir(stat.mode)) {
        directories.push(path);
      } else if (pyodide.FS.isFile(stat.mode)) {
        const relative = path.slice(`${purelib}/`.length);
        if (
          relative === "micropip" ||
          relative.startsWith("micropip/") ||
          /^micropip-[^/]+\.dist-info\//.test(relative)
        ) {
          continue;
        }
        files.push({
          path: relative,
          content_base64: encodeBase64(pyodide.FS.readFile(path)),
        });
      }
    }
  }
  files.sort((left, right) => left.path.localeCompare(right.path));
  return {
    schema: 1,
    pyodide_version: pyodideVersion,
    preinstall_packages: preinstallPackages,
    purelib,
    // micropip also records PyPI-installed names in loadedPackages. Only
    // Pyodide lockfile packages can be passed back to pyodide.loadPackage.
    pyodide_packages: Object.keys(pyodide.loadedPackages)
      .filter((name) => Object.hasOwn(pyodide.lockfile.packages, name))
      .sort(),
    files,
  };
}
