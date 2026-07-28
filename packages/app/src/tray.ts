import { defaultWindowIcon } from "@tauri-apps/api/app";
import type { Image } from "@tauri-apps/api/image";
import { CheckMenuItem, Menu, MenuItem, PredefinedMenuItem } from "@tauri-apps/api/menu";
import { TrayIcon, type TrayIconOptions } from "@tauri-apps/api/tray";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import { exit } from "@tauri-apps/plugin-process";
import { runtimeStateText } from "./labels";
import { useStore, type PageId } from "./store";
import { wsClient } from "./wsClient";

let tray: TrayIcon | null = null;
let lastKey: string | null = null;
let rebuilding = false;
let rebuildQueued = false;

/** 影响菜单结构 / 文案的最小状态指纹，避免每次快照广播都重建菜单。 */
function trayKey(): string {
  const { connected, snapshot } = useStore.getState();
  return [
    connected ? "1" : "0",
    snapshot?.runtimeState ?? "none",
    String(snapshot?.connection?.liveDeskEnabled ?? "none"),
  ].join("|");
}

async function openSettings(page?: PageId): Promise<void> {
  if (page) useStore.getState().setPage(page);
  const win = getCurrentWindow();
  await win.show();
  await win.setFocus();
}

/** 退出：先发 shutdown（bounded remote clear），等 result 或 1.5s，再退出进程。 */
async function quit(): Promise<void> {
  try {
    await Promise.race([
      wsClient.send({ cmd: "shutdown" }),
      new Promise<void>((resolve) => setTimeout(resolve, 1500)),
    ]);
  } catch {
    // core 可能已经不在了，忽略
  }
  await exit(0);
}

async function toggleAutostart(): Promise<void> {
  try {
    if (await isEnabled()) await disable();
    else await enable();
  } catch (error) {
    console.error("[tray] autostart toggle failed", error);
  }
  // 重建菜单以同步勾选状态（CheckMenuItem 点击会自行翻转显示）
  await rebuildMenu(true);
}

async function buildMenu(): Promise<Menu> {
  const { connected, snapshot } = useStore.getState();
  const statusText =
    connected && snapshot
      ? `状态：${runtimeStateText[snapshot.runtimeState]}`
      : "状态：核心服务未连接";

  const items: Array<MenuItem | CheckMenuItem | PredefinedMenuItem> = [
    await MenuItem.new({ id: "status", text: statusText, enabled: false }),
    await PredefinedMenuItem.new({ item: "Separator" }),
    await MenuItem.new({
      id: "open-settings",
      text: "打开设置",
      action: () => void openSettings(),
    }),
  ];

  const connection = snapshot?.connection ?? null;
  if (connection) {
    if (connection.liveDeskEnabled) {
      items.push(
        await MenuItem.new({
          id: "pause-sharing",
          text: "暂停分享",
          action: () => void wsClient.send({ cmd: "disableLiveDesk" }).catch(() => undefined),
        }),
      );
    } else {
      // 恢复分享必须回到设置窗口重新确认净化预览（fail-closed），
      // 所以这里只打开主窗口并跳到 Yohaku 页。
      items.push(
        await MenuItem.new({
          id: "resume-sharing",
          text: "恢复分享",
          action: () => void openSettings("yohaku"),
        }),
      );
    }
  }

  let autostartOn = false;
  try {
    autostartOn = await isEnabled();
  } catch {
    autostartOn = false;
  }
  items.push(
    await CheckMenuItem.new({
      id: "autostart",
      text: "开机自启动",
      checked: autostartOn,
      action: () => void toggleAutostart(),
    }),
    await PredefinedMenuItem.new({ item: "Separator" }),
    await MenuItem.new({ id: "quit", text: "退出", action: () => void quit() }),
  );

  return Menu.new({ items });
}

async function rebuildMenu(force: boolean): Promise<void> {
  if (!tray) return;
  const key = trayKey();
  if (!force && key === lastKey) return;
  if (rebuilding) {
    rebuildQueued = true;
    return;
  }
  rebuilding = true;
  try {
    lastKey = key;
    const menu = await buildMenu();
    await tray.setMenu(menu);
  } catch (error) {
    console.error("[tray] menu rebuild failed", error);
  } finally {
    rebuilding = false;
    if (rebuildQueued) {
      rebuildQueued = false;
      void rebuildMenu(true);
    }
  }
}

/** 在 main.tsx 启动时调用一次。 */
export async function initTray(): Promise<void> {
  if (tray) return;
  lastKey = trayKey();
  const menu = await buildMenu();
  const options: TrayIconOptions = {
    id: "yohaku-tray",
    tooltip: "Yohaku Companion",
    menu,
    showMenuOnLeftClick: true,
  };
  let icon: Image | null = null;
  try {
    icon = await defaultWindowIcon();
  } catch {
    icon = null;
  }
  if (icon) options.icon = icon;
  tray = await TrayIcon.new(options);
  useStore.subscribe(() => void rebuildMenu(false));
}
