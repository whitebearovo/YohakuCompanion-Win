import { negotiatePresence } from "./protocol/capabilities.js";
import {
  REQUIRED_PRESENCE_SCOPE,
  capabilitiesResponseSchema,
  pairingClaimResponseSchema,
  type PairingClaimData,
} from "./protocol/types.js";
import { PROTOCOL_CLIENT_VERSION } from "./protocol/wire.js";
import {
  CompanionHttpClient,
  CompanionServerConfiguration,
} from "./transport/httpClient.js";
import {
  CompanionPairingServerError,
  CompanionServerError,
} from "./transport/errors.js";

/**
 * Pairing client, ported from CompanionPairingClient.swift. The one-time
 * pairing code is consumed only AFTER (1) the credential store proved
 * writable and (2) capabilities negotiated successfully — never burn a code
 * for a token that could not be stored or used.
 */

export type PairingErrorCode =
  | "invalidPairingCode"
  | "invalidDeviceName"
  | "invalidServerUrl"
  | "clientUpdateRequired"
  | "serverFeatureUnavailable"
  | "invalidCapabilities"
  | "requiredScopeMissing"
  | "pairingRejected"
  | "network";

export class PairingError extends Error {
  override readonly name = "PairingError";
  constructor(
    readonly code: PairingErrorCode,
    /** Server error code when available (e.g. COMPANION_PAIRING_EXPIRED). */
    readonly serverCode?: string,
  ) {
    super(code);
  }
}

export interface PairingResult extends PairingClaimData {
  baseUrl: string;
  deviceName: string;
}

export async function claimPairing(
  baseUrl: string,
  deviceName: string,
  pairingCode: string,
  ensureCredentialStore: () => Promise<void>,
): Promise<PairingResult> {
  const code = pairingCode.trim();
  if (code.length < 1 || code.length > 32) {
    throw new PairingError("invalidPairingCode");
  }
  const name = deviceName.normalize("NFC").trim();
  if (name.length === 0 || [...name].length > 120) {
    throw new PairingError("invalidDeviceName");
  }

  let configuration: CompanionServerConfiguration;
  try {
    configuration = new CompanionServerConfiguration(baseUrl.trim());
  } catch {
    throw new PairingError("invalidServerUrl");
  }
  const http = new CompanionHttpClient(configuration);

  // Preflight 1: protected storage must be writable before consuming the code.
  await ensureCredentialStore();

  // Preflight 2: the protocol must be usable before consuming the code.
  let capabilities;
  try {
    capabilities = await http.execute({
      method: "GET",
      path: "/companion/capabilities",
      responseSchema: capabilitiesResponseSchema,
    });
  } catch {
    throw new PairingError("network");
  }
  const negotiation = negotiatePresence(capabilities.data, PROTOCOL_CLIENT_VERSION);
  if (negotiation.kind === "clientUpdateRequired") {
    throw new PairingError("clientUpdateRequired");
  }
  if (negotiation.kind === "schemaUnsupported" || negotiation.kind === "featureUnavailable") {
    throw new PairingError("serverFeatureUnavailable");
  }
  if (negotiation.kind === "invalidCapabilities") {
    throw new PairingError("invalidCapabilities");
  }

  let claim;
  try {
    claim = await http.execute({
      method: "POST",
      path: "/companion/pairings/claim",
      body: { deviceName: name, pairingCode: code },
      responseSchema: pairingClaimResponseSchema,
    });
  } catch (error) {
    const serverCode = extractPairingServerCode(error);
    if (serverCode !== null) {
      throw new PairingError("pairingRejected", serverCode);
    }
    throw new PairingError("network");
  }

  if (!claim.data.scopes.includes(REQUIRED_PRESENCE_SCOPE)) {
    throw new PairingError("requiredScopeMissing");
  }
  return { ...claim.data, baseUrl: configuration.baseUrl.toString(), deviceName: name };
}

function extractPairingServerCode(error: unknown): string | null {
  if (error instanceof CompanionPairingServerError) return error.code;
  if (error instanceof CompanionServerError) return error.envelope.error.code;
  return null;
}
