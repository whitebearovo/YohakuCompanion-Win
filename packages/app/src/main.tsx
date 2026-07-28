import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
import { App } from "./App";
import { useStore } from "./store";
import { initTray } from "./tray";
import { wsClient } from "./wsClient";
import "./styles.css";

const UPDATE_CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000;
let updateCheckRunning = false;

async function checkAndInstallUpdate(): Promise<void> {
  if (updateCheckRunning) return;
  updateCheckRunning = true;
  try {
    const update = await check();
    if (!update) return;
    await update.downloadAndInstall();
    await relaunch();
  } catch (error) {
    console.warn("[updater] automatic update check failed", error);
  } finally {
    updateCheckRunning = false;
  }
}

function startAutomaticUpdates(): void {
  window.setTimeout(() => void checkAndInstallUpdate(), 10_000);
  window.setInterval(() => void checkAndInstallUpdate(), UPDATE_CHECK_INTERVAL_MS);
}

const rootEl = document.getElementById("root");
if (!rootEl) throw new Error("missing #root");
createRoot(rootEl).render(<App />);

// 启动 WebSocket 客户端与托盘（各一次）
wsClient.start();
startAutomaticUpdates();
void initTray().catch((error) => console.error("[main] tray init failed", error));

const win = getCurrentWindow();

// 关闭 = 隐藏到托盘，应用继续在后台运行
void win.onCloseRequested((event) => {
  event.preventDefault();
  void win.hide();
});

// 非 --hidden 启动时显示主窗口
void invoke<boolean>("is_hidden_launch")
  .then((hidden) => {
    if (!hidden) {
      return win.show().then(() => win.setFocus());
    }
    return undefined;
  })
  .catch(() => win.show());

// core 崩溃且 shell 放弃重启时显示全局错误条
void listen("core-dead", () => {
  useStore.setState({ coreDead: true });
});
