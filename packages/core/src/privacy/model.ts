import type {
  ApplicationPrivacyRule,
  PrivacyConfig,
  PrivacyDefault,
  PrivacyOverride,
} from "@yohaku/shared";

/**
 * Text normalization used across the sanitization pipeline: Unicode NFC +
 * whitespace trim; an empty result means "value not present" (null).
 * Mirrors the macOS implementation's precomposedStringWithCanonicalMapping.
 */
export function normalizeText(value: string | null | undefined): string | null {
  if (value == null) return null;
  const normalized = value.normalize("NFC").trim();
  return normalized.length === 0 ? null : normalized;
}

/** Length in Unicode scalar values (code points), the unit all wire limits use. */
export function scalarLength(value: string): number {
  let count = 0;
  for (const _ of value) count += 1;
  return count;
}

export function resolveOverride(
  override: PrivacyOverride,
  fallback: PrivacyDefault,
): PrivacyDefault {
  return override === "inherit" ? fallback : override;
}

export function normalizeAppId(appId: string): string {
  return appId.normalize("NFC").trim().toLowerCase();
}

/** A rule with all-inherit overrides and no alias carries no information. */
export function isEmptyRule(rule: ApplicationPrivacyRule): boolean {
  return (
    rule.application === "inherit" &&
    rule.windowTitle === "inherit" &&
    rule.media === "inherit" &&
    normalizeText(rule.displayAlias) === null
  );
}

export function normalizedRule(rule: ApplicationPrivacyRule): ApplicationPrivacyRule {
  const alias = normalizeText(rule.displayAlias);
  const normalized: ApplicationPrivacyRule = {
    appId: normalizeAppId(rule.appId),
    application: rule.application,
    windowTitle: rule.windowTitle,
    media: rule.media,
  };
  if (alias !== null) normalized.displayAlias = alias;
  return normalized;
}

export function findRule(
  config: PrivacyConfig,
  appId: string,
): ApplicationPrivacyRule | null {
  const key = normalizeAppId(appId);
  return config.rules.find((rule) => normalizeAppId(rule.appId) === key) ?? null;
}

/**
 * Windows adaptation of the macOS "legacy hidden media names" fallback: a
 * media session that cannot be attributed to an executable is matched against
 * rules by the player's display name instead of an appId.
 */
export function findRuleByPlayerName(
  config: PrivacyConfig,
  playerName: string,
): ApplicationPrivacyRule | null {
  const key = normalizeAppId(playerName);
  if (key.length === 0) return null;
  return config.rules.find((rule) => normalizeAppId(rule.appId) === key) ?? null;
}

/** First-match exact mapping lookup for one mapping type. */
export function applyMapping(
  config: PrivacyConfig,
  type: "process_name" | "media_process_name",
  from: string,
): string | null {
  const hit = config.mappings.find((m) => m.type === type && m.from === from);
  return hit ? hit.to : null;
}
