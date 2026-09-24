const WORKSPACE_ROOT = "/workspace";
const MAX_OUTPUT_FILES = 64;
const MAX_OUTPUT_FILE_BYTES = 10 * 1024 * 1024;
const MAX_TOTAL_OUTPUT_FILE_BYTES = 50 * 1024 * 1024;

function listWorkspaceFiles(FS) {
  const directories = [WORKSPACE_ROOT];
  const files = [];

  while (directories.length > 0) {
    const directory = directories.pop();
    for (const name of FS.readdir(directory)) {
      if (name === "." || name === "..") continue;

      const absolutePath = `${directory}/${name}`;
      const stat = typeof FS.lstat === "function"
        ? FS.lstat(absolutePath)
        : FS.stat(absolutePath);
      if (FS.isDir(stat.mode)) {
        directories.push(absolutePath);
      } else if (FS.isFile(stat.mode)) {
        files.push({
          absolutePath,
          path: absolutePath.slice(`${WORKSPACE_ROOT}/`.length),
          size: stat.size,
        });
      }
    }
  }

  return files.sort((left, right) => left.path.localeCompare(right.path));
}

function encodeBase64(bytes) {
  const chunkSize = 32 * 1024;
  let binary = "";
  for (let offset = 0; offset < bytes.byteLength; offset += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunkSize));
  }
  return btoa(binary);
}

function mimeTypeForPath(path) {
  const extension = path.includes(".") ? path.split(".").at(-1).toLowerCase() : "";
  return {
    avif: "image/avif",
    bmp: "image/bmp",
    csv: "text/csv",
    gif: "image/gif",
    htm: "text/html",
    html: "text/html",
    jpeg: "image/jpeg",
    jpg: "image/jpeg",
    json: "application/json",
    pdf: "application/pdf",
    png: "image/png",
    svg: "image/svg+xml",
    tif: "image/tiff",
    tiff: "image/tiff",
    txt: "text/plain",
    webp: "image/webp",
  }[extension] ?? "application/octet-stream";
}

function collectCreatedFiles(FS, initialFiles) {
  const files = [];
  let totalBytes = 0;
  let truncated = false;

  for (const file of listWorkspaceFiles(FS)) {
    if (initialFiles.has(file.path)) continue;

    if (
      files.length >= MAX_OUTPUT_FILES ||
      file.size > MAX_OUTPUT_FILE_BYTES ||
      totalBytes + file.size > MAX_TOTAL_OUTPUT_FILE_BYTES
    ) {
      truncated = true;
      continue;
    }

    const bytes = FS.readFile(file.absolutePath);
    files.push({
      path: file.path,
      content_base64: encodeBase64(bytes),
      mime_type: mimeTypeForPath(file.path),
    });
    totalBytes += bytes.byteLength;
  }

  return { files, truncated };
}

(async () => {
  const request = globalThis.__request;
  const pyodide = globalThis.__pyodide;
  let initialFiles = new Set();
  globalThis.__resetCapture();

  try {
    pyodide.FS.mkdirTree("/workspace");
    for (const file of request.files) {
      const target = `/workspace/${file.path}`;
      const parent = target.slice(0, target.lastIndexOf("/"));
      pyodide.FS.mkdirTree(parent);
      const binary = atob(file.content_base64);
      const bytes = Uint8Array.from(binary, (character) =>
        character.charCodeAt(0)
      );
      pyodide.FS.writeFile(target, bytes);
    }
    pyodide.FS.chdir("/workspace");
    initialFiles = new Set(
      listWorkspaceFiles(pyodide.FS).map((file) => file.path),
    );

    if (globalThis.__packagePolicy.runtime_package_installation) {
      await pyodide.loadPackagesFromImports(request.code);
    }
    globalThis.__resetCapture();

    const value = await pyodide.runPythonAsync(request.code, {
      filename: "<run_python_code>",
    });

    let result = null;
    let resultTruncated = false;
    try {
      if (value !== undefined && value !== null) {
        const rendered = String(value);
        const encoded = new TextEncoder().encode(rendered);
        if (encoded.byteLength > globalThis.__resultLimitBytes) {
          result = new TextDecoder().decode(
            encoded.subarray(0, globalThis.__resultLimitBytes),
          );
          resultTruncated = true;
        } else {
          result = rendered;
        }
      }
    } finally {
      if (typeof value?.destroy === "function") value.destroy();
    }
    const createdFiles = collectCreatedFiles(pyodide.FS, initialFiles);

    return JSON.stringify({
      success: true,
      stdout: globalThis.__capture.stdout,
      stderr: globalThis.__capture.stderr,
      result,
      files: createdFiles.files,
      error: null,
      timed_out: false,
      output_truncated:
        globalThis.__capture.truncated ||
        resultTruncated ||
        createdFiles.truncated,
    });
  } catch (error) {
    const traceback = String(error?.message ?? error ?? "Unknown Python error");
    const errorLines = traceback.trimEnd().split("\n");
    const isPythonError =
      error?.name === "PythonError" ||
      traceback.includes("Traceback (most recent call last)");
    const createdFiles = collectCreatedFiles(pyodide.FS, initialFiles);
    return JSON.stringify({
      success: false,
      stdout: globalThis.__capture.stdout,
      stderr: globalThis.__capture.stderr,
      result: null,
      files: createdFiles.files,
      error: {
        kind: isPythonError ? "python" : "runtime",
        message: errorLines.at(-1) || traceback,
        traceback: isPythonError ? traceback : null,
      },
      timed_out: false,
      output_truncated:
        globalThis.__capture.truncated || createdFiles.truncated,
    });
  }
})()
