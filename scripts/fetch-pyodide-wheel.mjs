import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";

const assetDir = process.argv[2];
if (!assetDir) {
  throw new Error("Usage: node fetch-pyodide-wheel.mjs <pyodide-asset-dir>");
}

const packageJson = JSON.parse(
  await readFile(path.join(assetDir, "package.json"), "utf8"),
);
const lock = JSON.parse(
  await readFile(path.join(assetDir, "pyodide-lock.json"), "utf8"),
);
const micropip = lock.packages?.micropip;
if (!micropip?.file_name || !micropip?.sha256) {
  throw new Error("Pyodide lock file does not contain micropip wheel metadata");
}

const wheelPath = path.join(assetDir, micropip.file_name);
const wheelUrl = new URL(
  `https://cdn.jsdelivr.net/pyodide/v${packageJson.version}/full/${micropip.file_name}`,
);
const response = await fetch(wheelUrl);
if (!response.ok) {
  throw new Error(`Could not fetch ${wheelUrl}: HTTP ${response.status}`);
}

const wheel = Buffer.from(await response.arrayBuffer());
const sha256 = createHash("sha256").update(wheel).digest("hex");
if (sha256 !== micropip.sha256) {
  throw new Error(
    `SHA-256 mismatch for ${micropip.file_name}: expected ${micropip.sha256}, got ${sha256}`,
  );
}

await mkdir(assetDir, { recursive: true });
await writeFile(wheelPath, wheel, { flag: "wx" });
console.log(`Downloaded and verified ${micropip.file_name}`);
