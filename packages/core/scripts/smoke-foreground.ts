/**
 * Manual smoke test: prints the foreground stream for 15 seconds.
 * Run: pnpm smoke:foreground   (switch windows while it runs)
 */
import { ForegroundWatcher } from "../src/capture/foreground/ForegroundWatcher.js";

const watcher = new ForegroundWatcher();
watcher.onChange((info) => {
  console.log(
    "[change]",
    info === null
      ? "(none)"
      : `${info.appId} | ${info.displayName} | title=${JSON.stringify(info.windowTitle)}`,
  );
});
watcher.start();
console.log("watching foreground for 15s — switch windows now");
setTimeout(() => {
  const current = watcher.current();
  console.log(
    "[final current()]",
    current === null
      ? "(none)"
      : `${current.appId} | ${current.displayName} | title=${JSON.stringify(current.windowTitle)}`,
  );
  watcher.stop();
  process.exit(0);
}, 15_000);
