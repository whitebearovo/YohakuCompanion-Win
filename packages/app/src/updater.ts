import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type UpdaterStatus =
  | "idle"
  | "checking"
  | "downloading"
  | "upToDate"
  | "installed"
  | "error";

export type UpdateSource = "github" | "mirror";

export interface UpdateProgress {
  phase: "downloading" | "verifying" | "upToDate";
  downloaded: number;
  total: number | null;
  version: string | null;
}

type Listener = () => void;

let status: UpdaterStatus = "idle";
let errorMessage: string | null = null;
let inFlight: Promise<void> | null = null;
let progress: UpdateProgress | null = null;
let updateSource: UpdateSource = loadUpdateSource();
const listeners = new Set<Listener>();

function loadUpdateSource(): UpdateSource {
  try {
    return localStorage.getItem("yohaku-companion.update-source.v1") === "mirror"
      ? "mirror"
      : "github";
  } catch {
    return "github";
  }
}

function notify(next: UpdaterStatus, error: string | null = null): void {
  status = next;
  errorMessage = error;
  for (const listener of listeners) listener();
}

function notifyProgress(next: UpdateProgress | null): void {
  progress = next;
  for (const listener of listeners) listener();
}

export function getUpdaterStatus(): UpdaterStatus {
  return status;
}

export function getUpdaterError(): string | null {
  return errorMessage;
}

export function getUpdaterProgress(): UpdateProgress | null {
  return progress;
}

export function getUpdateSource(): UpdateSource {
  return updateSource;
}

export function setUpdateSource(source: UpdateSource): void {
  updateSource = source;
  try {
    localStorage.setItem("yohaku-companion.update-source.v1", source);
  } catch {
    // The selected source still applies for the current session.
  }
  for (const listener of listeners) listener();
}

export function subscribeUpdater(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export async function checkAndInstallUpdate(source: UpdateSource = updateSource): Promise<void> {
  if (inFlight) return inFlight;

  inFlight = (async () => {
    notify("checking");
    notifyProgress(null);
    try {
      await invoke("check_and_install_update", { source });
      if (status === "checking") notify("upToDate");
      if (status === "downloading") notify("installed");
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
  void listen<UpdateProgress>("updater-progress", ({ payload }) => {
    if (payload.phase === "upToDate") {
      notifyProgress(payload);
      notify("upToDate");
      return;
    }
    if (payload.phase === "downloading") {
      notifyProgress(payload);
      notify("downloading");
      return;
    }
    notifyProgress(payload);
    notify("downloading");
  });
  const intervalMs = 6 * 60 * 60 * 1000;
  window.setTimeout(() => {
    void checkAndInstallUpdate().catch(() => undefined);
  }, 10_000);
  window.setInterval(() => {
    void checkAndInstallUpdate().catch(() => undefined);
  }, intervalMs);
}
