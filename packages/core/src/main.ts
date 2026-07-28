import { randomBytes } from "node:crypto";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { CoreStateSnapshot } from "@yohaku/shared";
import { ForegroundWatcher } from "./capture/foreground/ForegroundWatcher.js";
import { selectMediaProvider } from "./capture/media/selectProvider.js";
import { PsSystemEventsProvider } from "./capture/system/PsSystemEventsProvider.js";
import type { MediaProvider } from "./capture/types.js";
import { CompanionService } from "./companion/service.js";
import { createCommandHandler } from "./ipc/handlers.js";
import { IpcServer } from "./ipc/server.js";
import { CaptureService } from "./privacy/captureService.js";
import { logger, recentLogLines } from "./runtime/logger.js";
import { SuspendDetector } from "./runtime/suspendDetector.js";
import { ConfigStore } from "./store/configStore.js";
import {
  selectCredentialStore,
  type CredentialStore,
} from "./store/credentials.js";
import { FileSequenceStore } from "./store/sequenceStore.js";

const APP_VERSION = "0.3.0";

/** Resolve the bundled PowerShell helper directory (dev vs staged layouts). */
function psDirectory(): string {
  const fromEnv = process.env.YOHAKU_PS_DIR;
  if (fromEnv !== undefined && fromEnv.length > 0) return fromEnv;
  const here = dirname(fileURLToPath(import.meta.url));
  for (const candidate of [
    join(here, "..", "resources", "ps"),
    join(here, "ps"),
  ]) {
    if (existsSync(candidate)) return candidate;
  }
  throw new Error("PowerShell helper directory not found");
}

async function main(): Promise<void> {
  const token = process.env.YOHAKU_IPC_TOKEN ?? randomBytes(24).toString("hex");
  if (process.env.YOHAKU_IPC_TOKEN === undefined) {
    // Standalone/dev launch without the shell: print the token so a local
    // client can connect. Never logged in shell-managed mode.
    console.log(JSON.stringify({ type: "dev-token", token }));
  }

  const config = new ConfigStore();
  const sequenceStore = new FileSequenceStore();

  let credentialsPromise: Promise<CredentialStore> | null = null;
  const credentials = (): Promise<CredentialStore> => {
    credentialsPromise ??= selectCredentialStore(config.get().credentialBackend).then(
      (store) => {
        if (config.get().credentialBackend !== store.backend) {
          config.update((c) => ({ ...c, credentialBackend: store.backend }));
        }
        return store;
      },
    );
    return credentialsPromise;
  };

  const foreground = new ForegroundWatcher();
  foreground.start();

  let mediaProvider: MediaProvider | null = null;
  try {
    mediaProvider = await selectMediaProvider(
      config.get().media.provider,
      join(psDirectory(), "smtc-provider.ps1"),
    );
  } catch {
    logger.error("main", "no media provider available; media capture disabled");
  }

  const capture = new CaptureService(
    foreground,
    () => mediaProvider,
    () => config.get().privacy,
  );

  const recentAppIds: string[] = [];
  const ipc = new IpcServer({
    token,
    getSnapshot: (): CoreStateSnapshot => {
      const c = config.get();
      const telemetry = service.publishTelemetry();
      return {
        version: APP_VERSION,
        runtimeState: service.runtimeState(),
        connection:
          c.connection === null
            ? null
            : {
                baseUrl: c.connection.baseUrl,
                deviceId: c.connection.deviceId,
                deviceName: c.connection.deviceName,
                scopes: c.connection.scopes,
                liveDeskEnabled: c.connection.liveDeskEnabled,
              },
        privacy: c.privacy,
        preview: service.currentPreview(),
        mediaProvider: {
          kind: mediaProvider?.kind ?? "none",
          healthy: mediaProvider?.healthy() ?? false,
        },
        recentAppIds: [...recentAppIds],
        lastPublishAt: telemetry.lastPublishAt,
        lastError: telemetry.lastError,
      };
    },
    onCommand: (command) => handler(command),
  });

  const service = new CompanionService({
    config,
    capture,
    sequenceStore,
    credentials,
    onChanged: () => ipc.broadcastState(),
  });

  const handler = createCommandHandler({
    config,
    service,
    requestShutdown: () => void gracefulExit(0),
  });

  // --- capture triggers -----------------------------------------------------
  foreground.onChange((info) => {
    if (info !== null) {
      const existing = recentAppIds.indexOf(info.appId);
      if (existing !== -1) recentAppIds.splice(existing, 1);
      recentAppIds.unshift(info.appId);
      recentAppIds.length = Math.min(recentAppIds.length, 10);
    }
    service.coordinator.requestFreshSnapshot();
    ipc.broadcastState();
  });
  mediaProvider?.onSemanticChange(() => {
    service.coordinator.requestFreshSnapshot();
  });

  // --- system events --------------------------------------------------------
  const systemEvents = new PsSystemEventsProvider(
    join(psDirectory(), "system-events.ps1"),
  );
  systemEvents.onLockOrSleep(() => service.handleSleepOrLock());
  systemEvents.onUnlockOrResume(() => service.handleWakeOrUnlock());
  await systemEvents.start();

  const suspendDetector = new SuspendDetector(() => {
    // Missed a suspend: the lease already expired remotely; renegotiate.
    const state = service.runtimeState();
    if (state === "active" || state === "degraded") {
      service.coordinator.start();
    }
  });
  suspendDetector.start();

  // --- IPC up, announce readiness ------------------------------------------
  const port = await ipc.start();
  console.log(JSON.stringify({ type: "ready", port }));

  service.start();

  // --- bounded shutdown -----------------------------------------------------
  let exiting = false;
  async function gracefulExit(code: number): Promise<void> {
    if (exiting) return;
    exiting = true;
    logger.info("main", "shutting down");
    suspendDetector.stop();
    foreground.stop();
    await Promise.race([
      (async () => {
        await service.shutdown();
        await systemEvents.stop();
        await mediaProvider?.stop();
        await ipc.stop();
      })(),
      new Promise((resolve) => setTimeout(resolve, 2000)),
    ]);
    process.exit(code);
  }

  for (const signal of ["SIGINT", "SIGTERM", "SIGBREAK", "SIGHUP"] as const) {
    process.on(signal, () => void gracefulExit(0));
  }
}

main().catch((error: unknown) => {
  logger.error("main", `fatal: ${(error as Error).message}`);
  process.exit(1);
});
