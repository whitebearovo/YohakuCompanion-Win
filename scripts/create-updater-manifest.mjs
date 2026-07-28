import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const bundleDir = join(
  process.cwd(),
  "packages",
  "app",
  "src-tauri",
  "target",
  "release",
  "bundle",
  "nsis",
);
const version = JSON.parse(readFileSync("package.json", "utf8")).version;
const signatureName = readdirSync(bundleDir).find((name) => name.endsWith(".nsis.zip.sig"));

if (!signatureName) {
  throw new Error(`No signed NSIS updater artifact found in ${bundleDir}`);
}

const archiveName = signatureName.slice(0, -4);
const repository = process.env.GITHUB_REPOSITORY ?? "whitebearovo/YohakuCompanion-Win";
const tag = process.env.GITHUB_REF_NAME ?? `v${version}`;
const baseUrl = `https://github.com/${repository}/releases/download/${tag}`;
const signature = readFileSync(join(bundleDir, signatureName), "utf8").trim();

const manifest = {
  version,
  notes: `Yohaku Companion ${version}`,
  pub_date: new Date().toISOString(),
  platforms: {
    "windows-x86_64": {
      signature,
      url: `${baseUrl}/${encodeURIComponent(archiveName).replace(/%2F/g, "/")}`,
    },
  },
};

writeFileSync(join(bundleDir, "latest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
