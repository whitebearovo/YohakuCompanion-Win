import { z } from "zod";
import {
  applicationPrivacyRuleSchema,
  privacyMappingSchema,
  privacyDefaultSchema,
} from "./privacy.js";
import { coreStateSnapshotSchema, previewSchema } from "./status.js";

/**
 * WebSocket IPC between the Tauri shell/WebView and the Node core service.
 * The core listens on 127.0.0.1:<random port>; the first client frame must be
 * a hello carrying the launch token. Snapshots never contain secrets or
 * un-sanitized capture values.
 */

export const helloMessageSchema = z.object({
  type: z.literal("hello"),
  token: z.string().min(1),
});
export type HelloMessage = z.infer<typeof helloMessageSchema>;

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

const base = { id: z.string().min(1) };

export const commandSchema = z.discriminatedUnion("cmd", [
  z.object({ ...base, cmd: z.literal("getState") }),
  z.object({
    ...base,
    cmd: z.literal("pair"),
    baseUrl: z.string().min(1),
    deviceName: z.string().min(1),
    pairingCode: z.string().min(1),
  }),
  z.object({ ...base, cmd: z.literal("unpair") }),
  z.object({ ...base, cmd: z.literal("requestPreview") }),
  z.object({
    ...base,
    cmd: z.literal("confirmConsent"),
    policyFingerprint: z.string().min(1),
  }),
  z.object({ ...base, cmd: z.literal("disableLiveDesk") }),
  z.object({
    ...base,
    cmd: z.literal("setSources"),
    application: z.boolean().optional(),
    media: z.boolean().optional(),
  }),
  z.object({ ...base, cmd: z.literal("setPrivacy"), patch: privacyPatchSchema }),
  z.object({ ...base, cmd: z.literal("upsertRule"), rule: applicationPrivacyRuleSchema }),
  z.object({ ...base, cmd: z.literal("deleteRule"), appId: z.string().min(1) }),
  z.object({ ...base, cmd: z.literal("setMappings"), mappings: z.array(privacyMappingSchema) }),
  z.object({ ...base, cmd: z.literal("shutdown") }),
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

export const serverMessageSchema = z.discriminatedUnion("type", [
  z.object({ type: z.literal("state"), snapshot: coreStateSnapshotSchema }),
  z.object({ type: z.literal("preview"), preview: previewSchema }),
  z.object({
    type: z.literal("result"),
    id: z.string(),
    ok: z.boolean(),
    error: ipcErrorCodeSchema.optional(),
  }),
]);
export type ServerMessage = z.infer<typeof serverMessageSchema>;

export const clientMessageSchema = z.union([helloMessageSchema, commandSchema]);
export type ClientMessage = z.infer<typeof clientMessageSchema>;
