import { z } from "zod";

/**
 * Privacy model, ported from YohakuCompanion (macOS) PresencePrivacyPolicy.swift.
 * `appId` replaces macOS bundle identifiers: it is the lowercased executable
 * file name of the process (e.g. "code.exe"). Media sessions that cannot be
 * mapped to an exe are matched by their normalized player display name instead.
 */

export const privacyDefaultSchema = z.enum(["share", "hide"]);
export type PrivacyDefault = z.infer<typeof privacyDefaultSchema>;

export const privacyOverrideSchema = z.enum(["inherit", "share", "hide"]);
export type PrivacyOverride = z.infer<typeof privacyOverrideSchema>;

export const applicationPrivacyRuleSchema = z.object({
  appId: z.string().min(1),
  application: privacyOverrideSchema,
  windowTitle: privacyOverrideSchema,
  media: privacyOverrideSchema,
  displayAlias: z.string().optional(),
});
export type ApplicationPrivacyRule = z.infer<typeof applicationPrivacyRuleSchema>;

export const privacyMappingSchema = z.object({
  type: z.enum(["process_name", "media_process_name"]),
  from: z.string().min(1),
  to: z.string().min(1),
});
export type PrivacyMapping = z.infer<typeof privacyMappingSchema>;

export const privacyConfigSchema = z.object({
  defaults: z.object({
    application: privacyDefaultSchema,
    windowTitle: privacyDefaultSchema,
    media: privacyDefaultSchema,
  }),
  rules: z.array(applicationPrivacyRuleSchema),
  mappings: z.array(privacyMappingSchema),
  shareWindowTitles: z.boolean(),
  ignoreNullArtist: z.boolean(),
  sources: z.object({
    application: z.boolean(),
    media: z.boolean(),
  }),
});
export type PrivacyConfig = z.infer<typeof privacyConfigSchema>;

/** New-installation defaults: window titles are private unless opted in. */
export function defaultPrivacyConfig(): PrivacyConfig {
  return {
    defaults: { application: "share", windowTitle: "hide", media: "share" },
    rules: [],
    mappings: [],
    shareWindowTitles: false,
    ignoreNullArtist: false,
    sources: { application: true, media: true },
  };
}
