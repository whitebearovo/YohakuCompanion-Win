import { mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { SequencePersistence } from "../companion/sequencer.js";
import { atomicWriteJson, dataDirectory } from "./configStore.js";

/**
 * sequence.json — per-device next-sequence persistence, deliberately a
 * separate file from config.json: correctness writes (reserve-before-send)
 * must not race the much chattier settings writes.
 */
export class FileSequenceStore implements SequencePersistence {
  private readonly path: string;

  constructor(directory: string = dataDirectory()) {
    mkdirSync(directory, { recursive: true });
    this.path = join(directory, "sequence.json");
  }

  private read(): Record<string, number> {
    try {
      const parsed: unknown = JSON.parse(readFileSync(this.path, "utf8"));
      if (parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)) {
        return parsed as Record<string, number>;
      }
    } catch {
      /* missing or corrupt -> empty; sequencer self-heals via reconcile */
    }
    return {};
  }

  async load(deviceId: string): Promise<number | null> {
    const value = this.read()[deviceId];
    return typeof value === "number" ? value : null;
  }

  async store(deviceId: string, next: number): Promise<void> {
    const all = this.read();
    all[deviceId] = next;
    atomicWriteJson(this.path, all);
  }

  async remove(deviceId: string): Promise<void> {
    const all = this.read();
    if (deviceId in all) {
      delete all[deviceId];
      atomicWriteJson(this.path, all);
    }
  }
}
