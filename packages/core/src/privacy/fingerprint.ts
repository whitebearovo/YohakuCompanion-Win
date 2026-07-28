import { createHash } from "node:crypto";
import type { PrivacyConfig } from "@yohaku/shared";
import { isEmptyRule, normalizedRule } from "./model.js";

/** Hashes the persisted privacy projection used by the consent gate. */
function canonicalize(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonicalize);
  if (value !== null && typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>)
      .filter(([, current]) => current !== undefined)
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
    return Object.fromEntries(entries.map(([key, current]) => [key, canonicalize(current)]));
  }
  return value;
}

export function policyFingerprint(config: PrivacyConfig): string {
  const rules = config.rules
    .map(normalizedRule)
    .filter((rule) => !isEmptyRule(rule))
    .sort((a, b) => (a.appId < b.appId ? -1 : a.appId > b.appId ? 1 : 0));
  const mappings = [...config.mappings].sort((a, b) => {
    const ka = `${a.type}\\0${a.from}`;
    const kb = `${b.type}\\0${b.from}`;
    return ka < kb ? -1 : ka > kb ? 1 : 0;
  });
  const projection = canonicalize({
    sources: config.sources,
    shareWindowTitles: config.shareWindowTitles,
    ignoreNullArtist: config.ignoreNullArtist,
    defaults: config.defaults,
    rules,
    mappings,
  });
  return createHash("sha256").update(JSON.stringify(projection)).digest("hex");
}

\n