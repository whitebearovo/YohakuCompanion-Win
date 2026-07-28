import { logger } from "../runtime/logger.js";
import { DpapiCredentialStore } from "./dpapiCredentialStore.js";
import { KeyringCredentialStore } from "./keyringCredentialStore.js";

/**
 * Device-token storage. Primary backend: Windows Credential Manager via
 * @napi-rs/keyring. Fallback: DPAPI-encrypted file (CurrentUser scope).
 * Tokens never touch config.json, logs, IPC payloads, or exports.
 */

export interface CredentialStore {
  readonly backend: "keyring" | "dpapi";
  get(deviceId: string): Promise<string | null>;
  set(deviceId: string, token: string): Promise<void>;
  delete(deviceId: string): Promise<void>;
}

export class CredentialStoreUnavailableError extends Error {
  override readonly name = "CredentialStoreUnavailableError";
}

/**
 * Probes backends with a set/get/delete round-trip on a sentinel entry.
 * `preferred` pins the backend that stored an existing token so a later
 * probe result cannot orphan it.
 */
export async function selectCredentialStore(
  preferred: "keyring" | "dpapi" | null,
): Promise<CredentialStore> {
  const candidates: CredentialStore[] =
    preferred === "dpapi"
      ? [new DpapiCredentialStore(), new KeyringCredentialStore()]
      : [new KeyringCredentialStore(), new DpapiCredentialStore()];
  for (const store of candidates) {
    if (await roundTrips(store)) {
      logger.info("credentials", `using ${store.backend} backend`);
      return store;
    }
    logger.warn("credentials", `${store.backend} backend unavailable`);
  }
  throw new CredentialStoreUnavailableError("no credential backend available");
}

async function roundTrips(store: CredentialStore): Promise<boolean> {
  const sentinel = "__yohaku_probe__";
  try {
    await store.set(sentinel, "probe");
    const read = await store.get(sentinel);
    await store.delete(sentinel);
    return read === "probe";
  } catch {
    return false;
  }
}
