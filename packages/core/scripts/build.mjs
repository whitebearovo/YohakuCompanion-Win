import { build } from "esbuild";

// Native/prebuilt packages stay external: they resolve from the staged
// node_modules that ships next to main.cjs inside Tauri resources.
await build({
  entryPoints: ["src/main.ts"],
  bundle: true,
  platform: "node",
  target: "node22",
  format: "cjs",
  outfile: "dist/main.cjs",
  sourcemap: false,
  external: ["koffi", "@napi-rs/keyring", "@coooookies/windows-smtc-monitor"],
  logLevel: "info",
});
