import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";

export type UpdaterStatus =
  | "idle"
  | "checking"
  | "downloading"
  | "upToDate"
  | "installed"
  | "error";

type Listener = () => void;

let status: UpdaterStatus = "idle";
let errorMessage: string | null = null;
let inFlight: Promise<void> | null = null;
const listeners = new Set<Listener>();

function notify(next: UpdaterStatus, error: string | null = null): void {
  status = next;
  errorMessage = error;
  for (const listener of listeners) listener();
}

export function getUpdaterStatus(): UpdaterStatus {
  return status;
}

export function getUpdaterError(): string | null {
  return errorMessage;
}

export function subscribeUpdater(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export async function checkAndInstallUpdate(): Promise<void> {
  if (inFlight) return inFlight;

  inFlight = (async () => {
    notify("checking");
    try {
      const update = await check();
      if (!update) {
        notify("upToDate");
        return;
      }

      notify("downloading");
      await update.downloadAndInstall();
      notify("installed");
      await relaunch();
    } catch (error) {
      const message = error instanceof Error ? error.message : "更新检查失败";
      notify("error", message);
      throw error;
    } finally {
      inFlight = null;
    }
  })();

  return inFlight;
}

export function startAutomaticUpdates(): void {
  const intervalMs = 6 * 60 * 60 * 1000;
  window.setTimeout(() => {
    void checkAndInstallUpdate().catch(() => undefined);
  }, 10_000);
  window.setInterval(() => {
    void checkAndInstallUpdate().catch(() => undefined);
  }, intervalMs);
}
