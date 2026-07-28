import { logger } from "../../runtime/logger.js";
import type { MediaProvider } from "../types.js";
import { SmtcNpmProvider } from "./SmtcNpmProvider.js";
import { SmtcPowershellProvider } from "./SmtcPowershellProvider.js";

export type MediaProviderPreference = "auto" | "npm" | "powershell";

/**
 * Selects and starts a media provider. In "auto", the in-process napi
 * provider is smoke-tested first (module load + one native call); any
 * failure degrades to the bundled PowerShell helper.
 */
export async function selectMediaProvider(
  preference: MediaProviderPreference,
  psScriptPath: string,
): Promise<MediaProvider> {
  if (preference !== "powershell") {
    const npm = new SmtcNpmProvider();
    try {
      await npm.start();
      return npm;
    } catch (error) {
      await npm.stop().catch(() => undefined);
      if (preference === "npm") throw error;
      logger.warn("media", "npm SMTC provider unavailable; falling back to PowerShell");
    }
  }
  const ps = new SmtcPowershellProvider(psScriptPath);
  await ps.start();
  return ps;
}
