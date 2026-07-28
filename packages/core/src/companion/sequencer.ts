import { MAXIMUM_SAFE_WIRE_INTEGER } from "./protocol/wire.js";

/**
 * Per-device monotonic sequence, ported from CompanionPresenceSequencer.
 * The invariant: the next sequence is PERSISTED before the current one is
 * returned for use. A crash between persist and send produces a legal gap;
 * a sequence number is never reused. reconcile() folds the server's
 * acceptedSequence back in (next = max(next, accepted + 1)).
 */

export interface SequencePersistence {
  load(deviceId: string): Promise<number | null>;
  store(deviceId: string, next: number): Promise<void>;
}

export class SequenceExhaustedError extends Error {
  override readonly name = "SequenceExhaustedError";
}

export class CompanionSequencer {
  private queue: Promise<unknown> = Promise.resolve();

  constructor(
    private readonly persistence: SequencePersistence,
    private readonly deviceId: string,
    private readonly pairingNextSequence: number,
  ) {}

  /** Serializes reserve/reconcile through a promise-chain mutex. */
  private enqueue<T>(task: () => Promise<T>): Promise<T> {
    const next = this.queue.then(task, task);
    this.queue = next.catch(() => undefined);
    return next;
  }

  private async currentNext(): Promise<number> {
    const stored = await this.persistence.load(this.deviceId);
    const valid =
      stored !== null &&
      Number.isInteger(stored) &&
      stored >= 0 &&
      stored <= MAXIMUM_SAFE_WIRE_INTEGER
        ? stored
        : null;
    return Math.max(this.pairingNextSequence, valid ?? this.pairingNextSequence);
  }

  /** Persists current+1, then returns current. Persistence IS the
   * linearization point — a failure to persist fails the reserve. */
  reserve(): Promise<number> {
    return this.enqueue(async () => {
      const current = await this.currentNext();
      if (current >= MAXIMUM_SAFE_WIRE_INTEGER) {
        throw new SequenceExhaustedError("sequence space exhausted");
      }
      await this.persistence.store(this.deviceId, current + 1);
      return current;
    });
  }

  reconcile(acceptedSequence: number): Promise<void> {
    return this.enqueue(async () => {
      if (
        !Number.isInteger(acceptedSequence) ||
        acceptedSequence < 0 ||
        acceptedSequence >= MAXIMUM_SAFE_WIRE_INTEGER
      ) {
        return;
      }
      const current = await this.currentNext();
      const next = Math.max(current, acceptedSequence + 1);
      if (next !== current) {
        await this.persistence.store(this.deviceId, next);
      }
    });
  }
}
