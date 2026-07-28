/**
 * Stages the Node core into Tauri resources:
 *   packages/app/src-tauri/resources/core/
 *     main.cjs          (esbuild bundle)
 *     ps/*.ps1          (PowerShell helpers)
 *     node_modules/     (production-only, flat — the three native deps)
 * Run AFTER `pnpm build:core`.
 */
import { execSync } from "node:child_process";
import { cpSync, mkdirSync, rmSync, writeFileSync, existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const coreDir = join(root, "packages", "core");
const stageDir = join(root, "packages", "app", "src-tauri", "resources", "core");

const bundle = join(coreDir, "dist", "main.cjs");
if (!existsSync(bundle)) {
  console.error("dist/main.cjs missing — run `pnpm build:core` first");
  process.exit(1);
}

rmSync(stageDir, { recursive: true, force: true });
mkdirSync(stageDir, { recursive: true });
cpSync(bundle, join(stageDir, "main.cjs"));
cpSync(join(coreDir, "resources", "ps"), join(stageDir, "ps"), { recursive: true });

// Runtime package.json with ONLY the external native deps, pinned to the
// exact versions the workspace uses.
const corePkg = JSON.parse(readFileSync(join(coreDir, "package.json"), "utf8"));
const runtimeDeps = {};
for (const name of ["koffi", "@napi-rs/keyring"]) {
  runtimeDeps[name] = corePkg.dependencies[name];
}
for (const name of ["@coooookies/windows-smtc-monitor"]) {
  runtimeDeps[name] = corePkg.optionalDependencies[name];
}
writeFileSync(
  join(stageDir, "package.json"),
  JSON.stringify(
    { name: "yohaku-core-runtime", private: true, dependencies: runtimeDeps },
    null,
    2,
  ),
);

console.log("installing production node_modules into stage...");
execSync("npm install --omit=dev --no-package-lock --no-audit --no-fund", {
  cwd: stageDir,
  stdio: "inherit",
});
console.log(`staged core at ${stageDir}`);
