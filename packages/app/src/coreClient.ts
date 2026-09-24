import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  coreStateSnapshotSchema,
  ipcErrorCodeSchema,
  type Command,
  type IpcErrorCode,
} from "@yohaku/shared";
import { useStore } from "./store";

/** 命令输入即 shared 的 Command union。 */
export type CommandInput = Command;

/** timeout / disconnected 是旧 WebSocket 客户端的遗留码，invoke 下不再产生。 */
export type SendErrorCode = IpcErrorCode | "timeout" | "disconnected";

/** send() 的失败统一以 IpcError 抛出，code 供 UI 映射为中文文案。 */
export class IpcError extends Error {
  readonly code: SendErrorCode;

  constructor(code: SendErrorCode) {
    super(`ipc error: ${code}`);
    this.name = "IpcError";
    this.code = code;
  }
}

/** Command.cmd → Tauri 命令名（invoke 目标）。 */
const COMMAND_NAMES: Record<CommandInput["cmd"], string> = {
  getState: "get_state",
  pair: "pair",
  unpair: "unpair",
  requestPreview: "request_preview",
  confirmConsent: "confirm_consent",
  disableLiveDesk: "disable_live_desk",
  setSources: "set_sources",
  setPrivacy: "set_privacy",
  upsertRule: "upsert_rule",
  deleteRule: "delete_rule",
  setMappings: "set_mappings",
  shutdown: "shutdown",
};

function toIpcError(error: unknown): IpcError {
  // Tauri 命令以 IpcErrorCode 字符串作为 Err 载荷；其余一律视为 internal
  if (typeof error === "string") {
    const code = ipcErrorCodeSchema.safeParse(error);
    if (code.success) return new IpcError(code.data);
  }
  return new IpcError("internal");
}

/**
 * 与进程内 Rust core 的客户端：核心随应用常驻，无握手、无重连。
 * listen("core-state") 收全量快照广播；命令走 invoke，错误码经 IpcError
 * 抛给调用方。快照仍用 zod 校验（与旧 WebSocket 客户端一致）。
 */
class CoreClient {
  private started = false;

  /** 在 main.tsx 启动时调用一次。 */
  start(): void {
    if (this.started) return;
    this.started = true;
    void listen("core-state", (event) => this.applySnapshot(event.payload));
    // 主动拉一次全量快照，避免错过 listen 建立前的广播
    void this.send({ cmd: "getState" }).catch(() => undefined);
  }

  /** 发送命令；失败以 IpcError 拒绝。 */
  async send(input: CommandInput): Promise<void> {
    const { cmd, ...args } = input;
    let result: unknown;
    try {
      result = await invoke<unknown>(COMMAND_NAMES[cmd], args);
    } catch (error) {
      throw toIpcError(error);
    }
    if (cmd === "getState") this.applySnapshot(result);
  }

  private applySnapshot(payload: unknown): void {
    const parsed = coreStateSnapshotSchema.safeParse(payload);
    if (!parsed.success) return;
    // 整体替换快照：UI 一切以 core 广播为准
    useStore.setState({ connected: true, snapshot: parsed.data });
  }
}

/** 单例。 */
export const coreClient = new CoreClient();
