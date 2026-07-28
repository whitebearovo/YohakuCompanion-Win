import type { CredentialStore } from "./credentials.js";

const SERVICE = "yohaku-companion-win";

interface KeyringEntry {
  getPassword(): string | null;
  setPassword(password: string): void;
  deletePassword(): boolean;
}

interface KeyringModule {
  Entry: new (service: string, account: string) => KeyringEntry;
}

/** Windows Credential Manager via @napi-rs/keyring (prebuilt N-API). */
export class KeyringCredentialStore implements CredentialStore {
  readonly backend = "keyring" as const;
  private modulePromise: Promise<KeyringModule> | null = null;

  private module(): Promise<KeyringModule> {
    this.modulePromise ??= import("@napi-rs/keyring") as Promise<KeyringModule>;
    return this.modulePromise;
  }

  async get(deviceId: string): Promise<string | null> {
    const { Entry } = await this.module();
    try {
      return new Entry(SERVICE, deviceId).getPassword();
    } catch {
      return null; // "no entry" surfaces as an error in some keyring versions
    }
  }

  async set(deviceId: string, token: string): Promise<void> {
    const { Entry } = await this.module();
    new Entry(SERVICE, deviceId).setPassword(token);
  }

  async delete(deviceId: string): Promise<void> {
    const { Entry } = await this.module();
    try {
      new Entry(SERVICE, deviceId).deletePassword();
    } catch {
      /* deleting a missing entry is fine */
    }
  }
}
