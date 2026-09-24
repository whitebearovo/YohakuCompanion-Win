import { z } from "zod";
import {
  applicationPrivacyRuleSchema,
  privacyMappingSchema,
  privacyDefaultSchema,
} from "./privacy.js";

/**
 * IPC between the WebView UI and the in-process Rust core: each `Command`
 * maps to a Tauri command (snake_case name), errors come back as
 * `IpcErrorCode` literals, and full state snapshots arrive via the
 * `core-state` event. Snapshots never contain secrets or un-sanitized
 * capture values.
 */

export const privacyPatchSchema = z.object({
  defaults: z
    .object({
      application: privacyDefaultSchema.optional(),
      windowTitle: privacyDefaultSchema.optional(),
      media: privacyDefaultSchema.optional(),
    })
    .optional(),
  shareWindowTitles: z.boolean().optional(),
  ignoreNullArtist: z.boolean().optional(),
});
export type PrivacyPatch = z.infer<typeof privacyPatchSchema>;

export const commandSchema = z.discriminatedUnion("cmd", [
  z.object({ cmd: z.literal("getState") }),
  z.object({
    cmd: z.literal("pair"),
    baseUrl: z.string().min(1),
    deviceName: z.string().min(1),
    pairingCode: z.string().min(1),
  }),
  z.object({ cmd: z.literal("unpair") }),
  z.object({ cmd: z.literal("requestPreview") }),
  z.object({
    cmd: z.literal("confirmConsent"),
    policyFingerprint: z.string().min(1),
  }),
  z.object({ cmd: z.literal("disableLiveDesk") }),
  z.object({
    cmd: z.literal("setSources"),
    application: z.boolean().optional(),
    media: z.boolean().optional(),
  }),
  z.object({ cmd: z.literal("setPrivacy"), patch: privacyPatchSchema }),
  z.object({ cmd: z.literal("upsertRule"), rule: applicationPrivacyRuleSchema }),
  z.object({ cmd: z.literal("deleteRule"), appId: z.string().min(1) }),
  z.object({ cmd: z.literal("setMappings"), mappings: z.array(privacyMappingSchema) }),
  z.object({ cmd: z.literal("shutdown") }),
]);
export type Command = z.infer<typeof commandSchema>;

export const ipcErrorCodeSchema = z.enum([
  "previewOutOfDate",
  "notPaired",
  "alreadyPaired",
  "pairingExpired",
  "pairingFailed",
  "requiredScopeMissing",
  "clientUpdateRequired",
  "serverFeatureUnavailable",
  "rateLimited",
  "validationFailed",
  "credentialStoreUnavailable",
  "network",
  "invalidInput",
  "internal",
]);
export type IpcErrorCode = z.infer<typeof ipcErrorCodeSchema>;

