import { z } from "zod";
import { privacyConfigSchema } from "./privacy.js";

/** Runtime state of the Live Desk coordinator, mirrored to the UI. */
export const runtimeStateSchema = z.enum([
  "notPaired",
  "disabled",
  "connecting",
  "active",
  "degraded",
  "suspended",
  "updateRequired",
  "serverFeatureUnavailable",
]);
export type RuntimeState = z.infer<typeof runtimeStateSchema>;

/** Non-secret pairing metadata. Never contains the device token. */
export const connectionSummarySchema = z.object({
  baseUrl: z.string(),
  deviceId: z.string(),
  deviceName: z.string(),
  scopes: z.array(z.string()),
  liveDeskEnabled: z.boolean(),
});
export type ConnectionSummary = z.infer<typeof connectionSummarySchema>;

/**
 * Consent projection: the exact sanitized values the user confirms before
 * Live Desk may publish. Deliberately excludes observedAt, media sessionId,
 * position and sampledAt so natural playback progress does not invalidate
 * consent, while any semantic change (track, app, pause state) does.
 */
export const previewProjectionSchema = z.object({
  application: z
    .object({
      displayName: z.string(),
      windowTitle: z.string().nullable(),
    })
    .nullable(),
  media: z
    .object({
      kind: z.enum(["music", "podcast", "video", "unknown"]),
      title: z.string().nullable(),
      artist: z.string().nullable(),
      album: z.string().nullable(),
      playerDisplayName: z.string().nullable(),
      playback: z.object({
        state: z.enum(["playing", "paused"]),
        durationSeconds: z.number().nullable(),
        rate: z.number(),
      }),
    })
    .nullable(),
});
export type PreviewProjection = z.infer<typeof previewProjectionSchema>;

export const previewSchema = z.object({
  projection: previewProjectionSchema,
  policyFingerprint: z.string(),
  observedAt: z.number(),
});
export type Preview = z.infer<typeof previewSchema>;

export const mediaProviderHealthSchema = z.object({
  kind: z.enum(["npm", "powershell", "none"]),
  healthy: z.boolean(),
  detail: z.string().optional(),
});
export type MediaProviderHealth = z.infer<typeof mediaProviderHealthSchema>;

/** Full non-sensitive state snapshot broadcast to the shell/UI. */
export const coreStateSnapshotSchema = z.object({
  version: z.string(),
  runtimeState: runtimeStateSchema,
  connection: connectionSummarySchema.nullable(),
  privacy: privacyConfigSchema,
  preview: previewSchema.nullable(),
  mediaProvider: mediaProviderHealthSchema,
  recentAppIds: z.array(z.string()),
  lastPublishAt: z.number().nullable(),
  lastError: z.string().nullable(),
});
export type CoreStateSnapshot = z.infer<typeof coreStateSnapshotSchema>;
