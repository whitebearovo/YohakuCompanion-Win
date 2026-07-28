import type { Command, IpcErrorCode } from "@yohaku/shared";
import type { CompanionService } from "../companion/service.js";
import { ServiceError } from "../companion/service.js";
import type { ConfigStore } from "../store/configStore.js";
import { isEmptyRule, normalizedRule } from "../privacy/model.js";

/**
 * Command router: privacy mutations go through ConfigStore (atomic write)
 * followed by policyMaybeChanged() so the consent gate and Live Desk always
 * observe the new fingerprint; connection commands delegate to the service.
 */

export interface HandlerDeps {
  config: ConfigStore;
  service: CompanionService;
  requestShutdown: () => void;
}

export function createCommandHandler(deps: HandlerDeps) {
  return async (command: Command): Promise<IpcErrorCode | null> => {
    try {
      await route(command, deps);
      return null;
    } catch (error) {
      if (error instanceof ServiceError) return error.code;
      return "internal";
    }
  };
}

async function route(command: Command, deps: HandlerDeps): Promise<void> {
  const { config, service } = deps;
  switch (command.cmd) {
    case "getState":
      return;
    case "pair":
      await service.pair(command.baseUrl, command.deviceName, command.pairingCode);
      return;
    case "unpair":
      await service.unpair();
      return;
    case "requestPreview":
      await service.refreshPreview();
      return;
    case "confirmConsent":
      await service.confirmConsent(command.policyFingerprint);
      return;
    case "disableLiveDesk":
      await service.disableLiveDesk();
      return;
    case "setSources":
      config.update((c) => ({
        ...c,
        privacy: {
          ...c.privacy,
          sources: {
            application: command.application ?? c.privacy.sources.application,
            media: command.media ?? c.privacy.sources.media,
          },
        },
      }));
      service.policyMaybeChanged();
      return;
    case "setPrivacy":
      config.update((c) => ({
        ...c,
        privacy: {
          ...c.privacy,
          defaults: {
            application: command.patch.defaults?.application ?? c.privacy.defaults.application,
            windowTitle: command.patch.defaults?.windowTitle ?? c.privacy.defaults.windowTitle,
            media: command.patch.defaults?.media ?? c.privacy.defaults.media,
          },
          shareWindowTitles: command.patch.shareWindowTitles ?? c.privacy.shareWindowTitles,
          ignoreNullArtist: command.patch.ignoreNullArtist ?? c.privacy.ignoreNullArtist,
        },
      }));
      service.policyMaybeChanged();
      return;
    case "upsertRule": {
      const rule = normalizedRule(command.rule);
      config.update((c) => {
        const rules = c.privacy.rules.filter((r) => r.appId !== rule.appId);
        if (!isEmptyRule(rule)) rules.push(rule);
        rules.sort((a, b) => (a.appId < b.appId ? -1 : 1));
        return { ...c, privacy: { ...c.privacy, rules } };
      });
      service.policyMaybeChanged();
      return;
    }
    case "deleteRule":
      config.update((c) => ({
        ...c,
        privacy: {
          ...c.privacy,
          rules: c.privacy.rules.filter(
            (r) => r.appId !== command.appId.toLowerCase(),
          ),
        },
      }));
      service.policyMaybeChanged();
      return;
    case "setMappings":
      config.update((c) => ({
        ...c,
        privacy: { ...c.privacy, mappings: command.mappings },
      }));
      service.policyMaybeChanged();
      return;
    case "shutdown":
      deps.requestShutdown();
      return;
  }
}
