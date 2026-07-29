import { useSyncExternalStore } from "react";

export interface BackgroundSettings {
  dataUrl: string | null;
  blur: number;
  opacity: number;
}

const STORAGE_KEY = "yohaku-companion.background.v1";
const DEFAULTS: BackgroundSettings = { dataUrl: null, blur: 18, opacity: 38 };
let settings = load();
const listeners = new Set<() => void>();

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function load(): BackgroundSettings {
  try {
    const parsed = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null") as Partial<BackgroundSettings> | null;
    if (!parsed || typeof parsed !== "object") return DEFAULTS;
    return {
      dataUrl: typeof parsed.dataUrl === "string" && parsed.dataUrl.startsWith("data:image/") ? parsed.dataUrl : null,
      blur: clamp(typeof parsed.blur === "number" ? parsed.blur : DEFAULTS.blur, 0, 40),
      opacity: clamp(typeof parsed.opacity === "number" ? parsed.opacity : DEFAULTS.opacity, 0, 100),
    };
  } catch {
    return DEFAULTS;
  }
}

function persist(): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(settings));
  } catch {
    // A large image should not break the settings page.
  }
  for (const listener of listeners) listener();
}

export function getBackgroundSettings(): BackgroundSettings {
  return settings;
}

export function subscribeBackground(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useBackgroundSettings(): BackgroundSettings {
  return useSyncExternalStore(subscribeBackground, getBackgroundSettings, getBackgroundSettings);
}

export function setBackgroundImage(dataUrl: string | null): void {
  settings = { ...settings, dataUrl };
  persist();
}

export function setBackgroundBlur(blur: number): void {
  settings = { ...settings, blur: clamp(blur, 0, 40) };
  persist();
}

export function setBackgroundOpacity(opacity: number): void {
  settings = { ...settings, opacity: clamp(opacity, 0, 100) };
  persist();
}
