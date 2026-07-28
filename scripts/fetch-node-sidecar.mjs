/**
 * Downloads the pinned official node.exe, verifies its SHA-256 against the
 * signed SHASUMS256.txt, and places it as the Tauri sidecar binary
 * (externalBin naming convention: <name>-<target-triple>.exe).
 */
import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const NODE_VERSION = "24.15.0";
const TRIPLE = "x86_64-pc-windows-msvc";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const outDir = join(root, "packages", "app", "src-tauri", "binaries");
const outFile = join(outDir, `yohaku-core-node-${TRIPLE}.exe`);

if (existsSync(outFile)) {
  console.log(`sidecar already present: ${outFile}`);
  process.exit(0);
}

const base = `https://nodejs.org/dist/v${NODE_VERSION}`;
console.log(`downloading node.exe v${NODE_VERSION}...`);
const [exeResponse, sumsResponse] = await Promise.all([
  fetch(`${base}/win-x64/node.exe`),
  fetch(`${base}/SHASUMS256.txt`),
]);
if (!exeResponse.ok || !sumsResponse.ok) {
  console.error("download failed");
  process.exit(1);
}
const exeBytes = Buffer.from(await exeResponse.arrayBuffer());
const sums = await sumsResponse.text();

const line = sums
  .split("\n")
  .find((l) => l.trim().endsWith("win-x64/node.exe"));
if (!line) {
  console.error("win-x64/node.exe not found in SHASUMS256.txt");
  process.exit(1);
}
const expected = line.trim().split(/\s+/)[0];
const actual = createHash("sha256").update(exeBytes).digest("hex");
if (actual !== expected) {
  console.error(`SHA-256 mismatch: expected ${expected}, got ${actual}`);
  process.exit(1);
}

mkdirSync(outDir, { recursive: true });
writeFileSync(outFile, exeBytes);
console.log(`verified and wrote ${outFile} (${(exeBytes.length / 1e6).toFixed(1)} MB)`);
