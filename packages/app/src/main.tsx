import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { App } from "./App";
import { initTray } from "./tray";
import { startAutomaticUpdates } from "./updater";
import { coreClient } from "./coreClient";
import "./styles.css";

const rootEl = document.getElementById("root");
if (!rootEl) throw new Error("missing #root");
createRoot(rootEl).render(<App />);

// 启动核心客户端与托盘（各一次）
coreClient.start();
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
