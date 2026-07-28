/**
 * Manual smoke test for media providers.
 * Run: pnpm smoke:media [npm|powershell|auto]  (default auto)
 * Play/pause something (Spotify, browser, etc.) while it runs (20s).
 */
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { selectMediaProvider, type MediaProviderPreference } from "../src/capture/media/selectProvider.js";

const preference = (process.argv[2] ?? "auto") as MediaProviderPreference;
const psScript = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "resources",
  "ps",
  "smtc-provider.ps1",
);

const provider = await selectMediaProvider(preference, psScript);
console.log(`provider: ${provider.kind}`);

provider.onSemanticChange(() => {
  void provider.getSnapshot().then((snapshot) => {
    console.log("[semantic-change]", JSON.stringify(snapshot));
  });
});

const initial = await provider.getSnapshot();
console.log("[initial]", JSON.stringify(initial));

setTimeout(() => {
  void (async () => {
    const final = await provider.getSnapshot();
    console.log("[final]", JSON.stringify(final));
    await provider.stop();
    process.exit(0);
  })();
}, 20_000);
