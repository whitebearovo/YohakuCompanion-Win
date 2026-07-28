import { describe, expect, it } from "vitest";
import {
  CompanionSequencer,
  SequenceExhaustedError,
  type SequencePersistence,
} from "../../src/companion/sequencer.js";
import { MAXIMUM_SAFE_WIRE_INTEGER } from "../../src/companion/protocol/wire.js";

class MemoryPersistence implements SequencePersistence {
  values = new Map<string, number>();
  log: Array<{ op: "load" | "store"; value?: number }> = [];
  failNextStore = false;

  async load(deviceId: string): Promise<number | null> {
    this.log.push({ op: "load" });
    return this.values.get(deviceId) ?? null;
  }
  async store(deviceId: string, next: number): Promise<void> {
    if (this.failNextStore) {
      this.failNextStore = false;
      throw new Error("disk full");
    }
    this.log.push({ op: "store", value: next });
    this.values.set(deviceId, next);
  }
}

const DEVICE = "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b";

describe("CompanionSequencer", () => {
  it("starts from pairingNextSequence and persists BEFORE returning", async () => {
    const p = new MemoryPersistence();
    const s = new CompanionSequencer(p, DEVICE, 5);
    const reserved = await s.reserve();
    expect(reserved).toBe(5);
    expect(p.values.get(DEVICE)).toBe(6);
    // store happened before reserve resolved
    expect(p.log.at(-1)).toEqual({ op: "store", value: 6 });
  });

  it("is monotonic across reserves", async () => {
    const p = new MemoryPersistence();
    const s = new CompanionSequencer(p, DEVICE, 0);
    expect(await s.reserve()).toBe(0);
    expect(await s.reserve()).toBe(1);
    expect(await s.reserve()).toBe(2);
  });

  it("a crash after persist produces a legal gap, never reuse", async () => {
    const p = new MemoryPersistence();
    const s1 = new CompanionSequencer(p, DEVICE, 0);
    await s1.reserve(); // 0 reserved, next=1 persisted; pretend crash before send
    const s2 = new CompanionSequencer(p, DEVICE, 0); // restart
    expect(await s2.reserve()).toBe(1); // gap at 0 is fine; no reuse
  });

  it("failed persistence fails the reserve (no sequence handed out)", async () => {
    const p = new MemoryPersistence();
    p.failNextStore = true;
    const s = new CompanionSequencer(p, DEVICE, 0);
    await expect(s.reserve()).rejects.toThrow("disk full");
    // next reserve still starts at 0 — nothing was consumed
    expect(await s.reserve()).toBe(0);
  });

  it("reconcile advances next to accepted+1, never backwards", async () => {
    const p = new MemoryPersistence();
    const s = new CompanionSequencer(p, DEVICE, 0);
    await s.reconcile(41);
    expect(await s.reserve()).toBe(42);
    await s.reconcile(10); // behind current — ignored
    expect(await s.reserve()).toBe(43);
  });

  it("invalid stored values are ignored (self-heal from pairing base)", async () => {
    const p = new MemoryPersistence();
    p.values.set(DEVICE, -7);
    const s = new CompanionSequencer(p, DEVICE, 3);
    expect(await s.reserve()).toBe(3);
  });

  it("pairingNextSequence acts as a floor over stale storage", async () => {
    const p = new MemoryPersistence();
    p.values.set(DEVICE, 2);
    const s = new CompanionSequencer(p, DEVICE, 10);
    expect(await s.reserve()).toBe(10);
  });

  it("throws when the sequence space is exhausted", async () => {
    const p = new MemoryPersistence();
    p.values.set(DEVICE, MAXIMUM_SAFE_WIRE_INTEGER);
    const s = new CompanionSequencer(p, DEVICE, 0);
    await expect(s.reserve()).rejects.toThrow(SequenceExhaustedError);
  });

  it("serializes concurrent reserves (unique, gapless when no crash)", async () => {
    const p = new MemoryPersistence();
    const s = new CompanionSequencer(p, DEVICE, 0);
    const reserved = await Promise.all(Array.from({ length: 20 }, () => s.reserve()));
    expect(new Set(reserved).size).toBe(20);
    expect([...reserved].sort((a, b) => a - b)).toEqual(
      Array.from({ length: 20 }, (_, i) => i),
    );
  });
});
