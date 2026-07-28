/**
 * Standalone mock Mix Space Core for manual end-to-end testing.
 * Run: pnpm mock-server   (listens on http://127.0.0.1:8787)
 * Pairing code: TEST-CODE (device name free-form).
 * Scenario knobs via env:
 *   MOCK_MIN_CLIENT=2.0.0     -> forces clientUpdateRequired
 *   MOCK_NO_LIVEDESK=1        -> features.liveDesk=false
 *   MOCK_FLAKY=1              -> every 3rd mutation gets a 500 (retryable)
 */
import { randomUUID } from "node:crypto";
import {
  MockCompanionServer,
  capabilitiesResponse,
  errorEnvelope,
  mutationSuccess,
  responseMeta,
} from "../test/helpers/mockServer.js";

const server = new MockCompanionServer();
let mutations = 0;

server.setFallback((req) => {
  const summary = `${req.method} ${req.path}`;
  if (req.path === "/companion/capabilities") {
    console.log(`[mock] ${summary}`);
    const caps = capabilitiesResponse({
      requestsPerMinute: 30,
      ...(process.env.MOCK_MIN_CLIENT ? { minimumClientVersion: process.env.MOCK_MIN_CLIENT } : {}),
      ...(process.env.MOCK_NO_LIVEDESK ? { liveDesk: false } : {}),
    });
    return caps;
  }
  if (req.path === "/companion/pairings/claim") {
    const body = req.json as { pairingCode?: string; deviceName?: string };
    console.log(`[mock] ${summary} device=${JSON.stringify(body.deviceName)}`);
    if (body.pairingCode !== "TEST-CODE") {
      return { status: 410, body: { error: { code: "COMPANION_PAIRING_EXPIRED" } } };
    }
    return {
      status: 200,
      body: {
        meta: responseMeta(randomUUID()),
        data: {
          deviceId: randomUUID(),
          deviceToken: `mock-token-${randomUUID()}`,
          scopes: ["companion:presence:write"],
          nextSequence: 0,
        },
      },
    };
  }
  if (req.path === "/companion/presence" || req.path === "/companion/presence/clear") {
    mutations += 1;
    const meta = (req.json as { meta: { sequence: number } }).meta;
    if (process.env.MOCK_FLAKY === "1" && mutations % 3 === 0) {
      console.log(`[mock] ${summary} seq=${meta.sequence} -> 500 (flaky)`);
      return errorEnvelope(req, 500, "INTERNAL_ERROR", { retryable: true });
    }
    const data = (req.json as { data: Record<string, unknown> }).data;
    console.log(
      `[mock] ${summary} seq=${meta.sequence} ${JSON.stringify(data).slice(0, 200)}`,
    );
    return mutationSuccess(req);
  }
  console.log(`[mock] ${summary} -> 404`);
  return { status: 404, body: { error: { code: "HTTP_ERROR" } } };
});

await server.start();
console.log(`mock mx-core listening at ${server.baseUrl}`);
console.log(`pair with: baseUrl=${server.baseUrl}  code=TEST-CODE`);
