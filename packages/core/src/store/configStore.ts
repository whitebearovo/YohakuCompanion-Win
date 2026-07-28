import { closeSync, fsyncSync, mkdirSync, openSync, renameSync, writeSync, readFileSync, copyFileSync } from "node:fs";
import { join } from "node:path";
import { z } from "zod";
import {
  defaultPrivacyConfig,
  privacyConfigSchema,
  type PrivacyConfig,
} from "@yohaku/shared";
import { logger } from "../runtime/logger.js";

/**
 * config.json — non-secret configuration only. The device token NEVER lives
 * here. Atomic writes (tmp + fsync + rename); a corrupt file is preserved as
 * .bak and replaced with defaults (fail-closed: losing `connection` means
 * "not paired", never "publishing enabled").
 */

export const storedConnectionSchema = z.object({
  baseUrl: z.string(),
  deviceId: z.string(),
  deviceName: z.string(),
  scopes: z.array(z.string()),
  pairingNextSequence: z.number().int().min(0),
  liveDeskEnabled: z.boolean(),
});
export type StoredConnection = z.infer<typeof storedConnectionSchema>;

export const coreConfigSchema = z.object({
  version: z.literal(1),
  privacy: privacyConfigSchema,
  connection: storedConnectionSchema.nullable(),
  media: z.object({ provider: z.enum(["auto", "npm", "powershell"]) }),
  credentialBackend: z.enum(["keyring", "dpapi"]).nullable(),
});
export type CoreConfig = z.infer<typeof coreConfigSchema>;

export function defaultCoreConfig(): CoreConfig {
  return {
    version: 1,
    privacy: defaultPrivacyConfig(),
    connection: null,
    media: { provider: "auto" },
    credentialBackend: null,
  };
}

export function dataDirectory(): string {
  const override = process.env.YOHAKU_DATA_DIR;
  if (override !== undefined && override.length > 0) return override;
  const appData = process.env.APPDATA;
  if (appData === undefined || appData.length === 0) {
    throw new Error("APPDATA is not set");
  }
  return join(appData, "yohaku-companion-win");
}

export function atomicWriteJson(path: string, value: unknown): void {
  const tmp = `${path}.tmp`;
  const fd = openSync(tmp, "w");
  try {
    writeSync(fd, JSON.stringify(value, null, 2));
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
  renameSync(tmp, path);
}

export class ConfigStore {
  private config: CoreConfig;
  private readonly path: string;
  private readonly listeners = new Set<(config: CoreConfig) => void>();

  constructor(directory: string = dataDirectory()) {
    mkdirSync(directory, { recursive: true });
    this.path = join(directory, "config.json");
    this.config = this.load();
  }

  get(): CoreConfig {
    return this.config;
  }

  get privacy(): PrivacyConfig {
    return this.config.privacy;
  }

  onChange(callback: (config: CoreConfig) => void): () => void {
    this.listeners.add(callback);
    return () => this.listeners.delete(callback);
  }

  update(mutate: (config: CoreConfig) => CoreConfig): CoreConfig {
    const next = coreConfigSchema.parse(mutate(structuredClone(this.config)));
    atomicWriteJson(this.path, next);
    this.config = next;
    for (const listener of this.listeners) listener(next);
    return next;
  }

  private load(): CoreConfig {
    let raw: string;
    try {
      raw = readFileSync(this.path, "utf8");
    } catch {
      return defaultCoreConfig();
    }
    try {
      return coreConfigSchema.parse(JSON.parse(raw));
    } catch {
      logger.error("config", "config.json corrupt; backing up and using defaults");
      try {
        copyFileSync(this.path, `${this.path}.bak`);
      } catch {
        /* backup is best-effort */
      }
      return defaultCoreConfig();
    }
  }
}
