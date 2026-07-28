import type { PrivacyConfig } from "@yohaku/shared";
import {
  findRule,
  findRuleByPlayerName,
  normalizeText,
  resolveOverride,
} from "./model.js";

/**
 * Privacy decision engine, ported from PresencePrivacyPolicy.swift.
 * Decisions always use the ORIGINAL (pre-mapping) identifiers. Hide wins over
 * everything; a hidden application never leaks its alias either.
 */

export interface ProcessDecision {
  sharesApplication: boolean;
  /**
   * Rule-level window title consent. The sanitizer additionally requires the
   * global shareWindowTitles switch — a title leaves the process only when the
   * app is not hidden AND the rule resolves to share AND the global switch is on.
   */
  sharesWindowTitle: boolean;
  displayAlias: string | null;
}

export function processDecision(
  config: PrivacyConfig,
  appId: string,
): ProcessDecision {
  const rule = findRule(config, appId);
  const hidden =
    resolveOverride(rule?.application ?? "inherit", config.defaults.application) ===
    "hide";
  const windowShared =
    resolveOverride(rule?.windowTitle ?? "inherit", config.defaults.windowTitle) ===
    "share";
  return {
    sharesApplication: !hidden,
    sharesWindowTitle: !hidden && windowShared,
    displayAlias: hidden ? null : normalizeText(rule?.displayAlias),
  };
}

export interface MediaDecision {
  sharesMedia: boolean;
  displayAlias: string | null;
}

export function mediaDecision(
  config: PrivacyConfig,
  appId: string | null,
  playerName: string,
): MediaDecision {
  const rule =
    appId !== null ? findRule(config, appId) : findRuleByPlayerName(config, playerName);
  const hidden =
    resolveOverride(rule?.media ?? "inherit", config.defaults.media) === "hide";
  return {
    sharesMedia: !hidden,
    displayAlias: hidden ? null : normalizeText(rule?.displayAlias),
  };
}
