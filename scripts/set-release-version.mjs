import { readFileSync, writeFileSync } from "node:fs";

const tag = process.env.RELEASE_TAG ?? "";
const match = /^v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)$/.exec(tag);

if (!match) {
  throw new Error(`Release ref must be a SemVer tag such as v0.3.0, received: ${tag || "empty"}`);
}

const version = match[1];

function replaceJsonVersion(path) {
  const source = readFileSync(path, "utf8");
  const versionPattern = /(^\s*"version"\s*:\s*")[^"]+("\s*,?)/m;
  if (!versionPattern.test(source)) {
    throw new Error(`Could not update version in ${path}`);
  }
  writeFileSync(path, source.replace(versionPattern, `$1${version}$2`));
}

for (const path of [
  "package.json",
  "packages/app/package.json",
  "packages/core/package.json",
  "packages/shared/package.json",
  "packages/app/src-tauri/tauri.conf.json",
]) {
  replaceJsonVersion(path);
}

const cargoPath = "packages/app/src-tauri/Cargo.toml";
const cargo = readFileSync(cargoPath, "utf8");
const cargoVersionPattern = /^(version\s*=\s*")[^"]+("\s*$)/m;
if (!cargoVersionPattern.test(cargo)) {
  throw new Error(`Could not update package version in ${cargoPath}`);
}
const updatedCargo = cargo.replace(cargoVersionPattern, `$1${version}$2`);
writeFileSync(cargoPath, updatedCargo);

const mainPath = "packages/core/src/main.ts";
const main = readFileSync(mainPath, "utf8");
const mainVersionPattern = /(const APP_VERSION = ")[^"]+(";)/;
if (!mainVersionPattern.test(main)) {
  throw new Error(`Could not update APP_VERSION in ${mainPath}`);
}
const updatedMain = main.replace(mainVersionPattern, `$1${version}$2`);
writeFileSync(mainPath, updatedMain);

console.log(`Release version set to ${version} from ${tag}`);
