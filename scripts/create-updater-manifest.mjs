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
const signatureName = readdirSync(bundleDir).find(
  (name) => /(?:\.exe|\.zip)\.sig$/i.test(name),
);

if (!signatureName) {
  const files = readdirSync(bundleDir).join(", ");
  throw new Error(
    `No signed Windows updater artifact found in ${bundleDir}. ` +
      `Configure TAURI_SIGNING_PRIVATE_KEY. Files present: ${files || "none"}`,
  );
}

const archiveName = signatureName.replace(/\.sig$/, "");
// GitHub normalizes spaces in uploaded release asset names to dots. The
// signature remains valid because it covers the file bytes, not this URL name.
const releaseAssetName = archiveName.replace(/\s+/g, ".");
const repository = process.env.GITHUB_REPOSITORY ?? "whitebearovo/YohakuCompanion-Win";
const tag = process.env.RELEASE_TAG ?? process.env.GITHUB_REF_NAME ?? `v${version}`;
const baseUrl = `https://github.com/${repository}/releases/download/${tag}`;
const signature = readFileSync(join(bundleDir, signatureName), "utf8").trim();

const manifest = {
  version,
  notes: `Yohaku Companion ${version}`,
  pub_date: new Date().toISOString(),
  platforms: {
    "windows-x86_64": {
      signature,
      url: `${baseUrl}/${encodeURIComponent(releaseAssetName).replace(/%2F/g, "/")}`,
    },
  },
};

writeFileSync(join(bundleDir, "latest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
