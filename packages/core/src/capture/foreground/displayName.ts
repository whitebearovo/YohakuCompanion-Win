import { execFile } from "node:child_process";
import { statSync } from "node:fs";
import { basename } from "node:path";
import { logger } from "../../runtime/logger.js";

/**
 * Friendly display names for executables. The authoritative source is the
 * exe's version resource FileDescription, read via a cached one-shot
 * PowerShell query (cheap: once per exe path + mtime). Until resolution
 * completes — or when it fails — the capitalized exe stem is used.
 */

const cache = new Map<string, string>(); // key: `${path}|${mtimeMs}`
const pending = new Set<string>();

export function exeStemDisplayName(exePathOrAppId: string): string {
  const base = basename(exePathOrAppId).replace(/\.exe$/i, "");
  if (base.length === 0) return exePathOrAppId;
  return base.charAt(0).toUpperCase() + base.slice(1);
}

function cacheKey(exePath: string): string | null {
  try {
    return `${exePath.toLowerCase()}|${statSync(exePath).mtimeMs}`;
  } catch {
    return null;
  }
}

/**
 * Returns the best currently-known display name and kicks off background
 * resolution on a cache miss; `onResolved` fires when a better name lands.
 */
export function displayNameFor(
  exePath: string | null,
  appId: string,
  onResolved?: (name: string) => void,
): string {
  const fallback = exeStemDisplayName(appId);
  if (exePath === null) return fallback;
  const key = cacheKey(exePath);
  if (key === null) return fallback;
  const cached = cache.get(key);
  if (cached !== undefined) return cached;

  if (!pending.has(key)) {
    pending.add(key);
    resolveFileDescription(exePath)
      .then((description) => {
        const name = description ?? fallback;
        cache.set(key, name);
        if (description !== null) onResolved?.(name);
      })
      .catch(() => {
        cache.set(key, fallback);
      })
      .finally(() => {
        pending.delete(key);
      });
  }
  return fallback;
}

function resolveFileDescription(exePath: string): Promise<string | null> {
  return new Promise((resolve, reject) => {
    // -LiteralPath avoids wildcard interpretation; path is passed as an
    // argument, not interpolated into the script text.
    const script =
      "param([string]$p); (Get-Item -LiteralPath $p).VersionInfo.FileDescription";
    execFile(
      "powershell.exe",
      ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command",
        `& { ${script} }`, exePath],
      { windowsHide: true, timeout: 10_000 },
      (error, stdout) => {
        if (error) {
          logger.debug("displayName", "FileDescription query failed");
          reject(error);
          return;
        }
        const value = stdout.trim();
        resolve(value.length > 0 ? value : null);
      },
    );
  });
}
