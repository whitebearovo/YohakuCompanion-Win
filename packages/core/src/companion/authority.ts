import type { PresenceClient } from "./presenceClient.js";
import type { CompanionSequencer } from "./sequencer.js";

export interface PresenceAuthority {
  client: PresenceClient;
  sequencer: CompanionSequencer;
}

/**
 * One ordered writer per paired (baseUrl, deviceId), ported from
 * CompanionPresenceAuthorityRegistry. Renegotiation across sleep/wake reuses
 * the same client+sequencer so a wake snapshot can never overtake an
 * in-flight clear's sequence state. Discarded on capability rejection,
 * unpair, or pairing replacement — callers must drain in-flight work first.
 */
export class AuthorityRegistry {
  private current: { key: string; authority: PresenceAuthority } | null = null;

  resolve(
    baseUrl: string,
    deviceId: string,
    factory: () => PresenceAuthority,
  ): PresenceAuthority {
    const key = `${baseUrl}|${deviceId}`;
    if (this.current !== null && this.current.key === key) {
      return this.current.authority;
    }
    this.current = { key, authority: factory() };
    return this.current.authority;
  }

  peek(): PresenceAuthority | null {
    return this.current?.authority ?? null;
  }

  discard(): void {
    this.current = null;
  }
}
