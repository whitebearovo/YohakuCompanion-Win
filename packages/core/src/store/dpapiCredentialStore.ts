import { execFile } from "node:child_process";
import { mkdirSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import type { CredentialStore } from "./credentials.js";
import { atomicWriteJson, dataDirectory } from "./configStore.js";

/**
 * DPAPI fallback backend: tokens are encrypted with
 * System.Security.Cryptography.ProtectedData (CurrentUser scope) via a
 * PowerShell one-shot, and stored base64-encoded in credentials.bin.json.
 * Secrets travel over stdin only — never on a command line.
 */

function runDpapi(mode: "protect" | "unprotect", input: string): Promise<string> {
  const script = `
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Security
$stdin = [Console]::In.ReadToEnd().Trim()
if ("${mode}" -eq "protect") {
  $bytes = [Text.Encoding]::UTF8.GetBytes($stdin)
  $out = [Security.Cryptography.ProtectedData]::Protect($bytes, $null, "CurrentUser")
  [Console]::Out.Write([Convert]::ToBase64String($out))
} else {
  $bytes = [Convert]::FromBase64String($stdin)
  $out = [Security.Cryptography.ProtectedData]::Unprotect($bytes, $null, "CurrentUser")
  [Console]::Out.Write([Text.Encoding]::UTF8.GetString($out))
}
`;
  return new Promise((resolve, reject) => {
    const child = execFile(
      "powershell.exe",
      ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script],
      { windowsHide: true, timeout: 15_000, maxBuffer: 1024 * 1024 },
      (error, stdout) => {
        if (error) reject(error);
        else resolve(stdout.trim());
      },
    );
    child.stdin?.end(input, "utf8");
  });
}

export class DpapiCredentialStore implements CredentialStore {
  readonly backend = "dpapi" as const;
  private readonly path: string;

  constructor(directory: string = dataDirectory()) {
    mkdirSync(directory, { recursive: true });
    this.path = join(directory, "credentials.bin.json");
  }

  private read(): Record<string, string> {
    try {
      const parsed: unknown = JSON.parse(readFileSync(this.path, "utf8"));
      if (parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)) {
        return parsed as Record<string, string>;
      }
    } catch {
      /* missing file -> empty */
    }
    return {};
  }

  async get(deviceId: string): Promise<string | null> {
    const blob = this.read()[deviceId];
    if (blob === undefined) return null;
    try {
      return await runDpapi("unprotect", blob);
    } catch {
      return null; // undecryptable (different user/machine) -> treat as absent
    }
  }

  async set(deviceId: string, token: string): Promise<void> {
    const blob = await runDpapi("protect", token);
    if (blob.length === 0) throw new Error("DPAPI protect produced no output");
    const all = this.read();
    all[deviceId] = blob;
    atomicWriteJson(this.path, all);
  }

  async delete(deviceId: string): Promise<void> {
    const all = this.read();
    if (deviceId in all) {
      delete all[deviceId];
      if (Object.keys(all).length === 0) {
        rmSync(this.path, { force: true });
      } else {
        atomicWriteJson(this.path, all);
      }
    }
  }
}
