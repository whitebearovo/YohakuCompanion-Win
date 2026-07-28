import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { serverMessageSchema, type Command, type IpcErrorCode } from "@yohaku/shared";
import { useStore } from "./store";

type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;

/** 去掉 id 的命令输入：id 由 wsClient 生成，用于关联 result 消息。 */
export type CommandInput = DistributiveOmit<Command, "id">;

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

interface PendingCommand {
  resolve: () => void;
  reject: (error: IpcError) => void;
  timer: ReturnType<typeof setTimeout>;
}

const SEND_TIMEOUT_MS = 10_000;
const INITIAL_BACKOFF_MS = 500;
const MAX_BACKOFF_MS = 10_000;
const ENDPOINT_RETRY_MS = 1_000;

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * 与 Node core 的 WebSocket 客户端：
 * invoke("get_core_endpoint") 拿 {port, token}（未就绪则监听 "core-ready" +
 * 1s 轮询）→ 连接 → 首帧 hello(token) → 收 state/preview/result。
 * 断线指数退避重连（上限 10s），重连后重新握手。
 */
class WsClient {
  /** 敏感值：token 只保存在本模块内存中，绝不进入 store 或组件。 */
  private token = "";
  private ws: WebSocket | null = null;
  private started = false;
  private connecting = false;
  private backoffMs = INITIAL_BACKOFF_MS;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private readonly pending = new Map<string, PendingCommand>();

  /** 在 main.tsx 启动时调用一次。 */
  start(): void {
    if (this.started) return;
    this.started = true;
    void listen("core-ready", () => this.onCoreReady());
    void this.connect();
  }

  /** 发送命令，result.ok 时 resolve；错误 / 超时 / 断线时以 IpcError 拒绝。 */
  send(input: CommandInput): Promise<void> {
    const ws = this.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      return Promise.reject(new IpcError("disconnected"));
    }
    const id = crypto.randomUUID();
    return new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new IpcError("timeout"));
      }, SEND_TIMEOUT_MS);
      this.pending.set(id, { resolve, reject, timer });
      try {
        ws.send(JSON.stringify({ ...input, id }));
      } catch {
        clearTimeout(timer);
        this.pending.delete(id);
        reject(new IpcError("disconnected"));
      }
    });
  }

  private onCoreReady(): void {
    this.backoffMs = INITIAL_BACKOFF_MS;
    if (this.ws && this.ws.readyState === WebSocket.OPEN) return;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    void this.connect();
  }

  private async connect(): Promise<void> {
    if (this.connecting) return;
    if (
      this.ws &&
      (this.ws.readyState === WebSocket.CONNECTING || this.ws.readyState === WebSocket.OPEN)
    ) {
      return;
    }
    this.connecting = true;
    try {
      let endpoint: { port: number; token: string } | null = null;
      while (endpoint === null) {
        try {
          endpoint = await invoke<{ port: number; token: string }>("get_core_endpoint");
        } catch {
          // core 未就绪（"core-not-ready"）："core-ready" 事件 + 1s 轮询双保险
          await delay(ENDPOINT_RETRY_MS);
        }
      }
      this.token = endpoint.token;
      this.open(endpoint.port);
    } finally {
      this.connecting = false;
    }
  }

  private open(port: number): void {
    const ws = new WebSocket(`ws://127.0.0.1:${port}`);
    this.ws = ws;
    ws.onopen = () => {
      this.backoffMs = INITIAL_BACKOFF_MS;
      ws.send(JSON.stringify({ type: "hello", token: this.token }));
      useStore.setState({ connected: true });
      // 握手后立刻拉一次完整快照，避免错过握手前的广播
      void this.send({ cmd: "getState" }).catch(() => undefined);
    };
    ws.onmessage = (event) => this.handleMessage(event.data);
    ws.onclose = () => {
      if (this.ws === ws) this.handleDisconnect();
    };
    ws.onerror = () => {
      // 出错后 onclose 紧随其后，统一在那里处理
    };
  }

  private handleDisconnect(): void {
    this.ws = null;
    useStore.setState({ connected: false });
    this.failAllPending();
    this.scheduleReconnect(this.backoffMs);
    this.backoffMs = Math.min(this.backoffMs * 2, MAX_BACKOFF_MS);
  }

  private scheduleReconnect(delayMs: number): void {
    if (this.reconnectTimer !== null) return;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      void this.connect();
    }, delayMs);
  }

  private failAllPending(): void {
    for (const entry of this.pending.values()) {
      clearTimeout(entry.timer);
      entry.reject(new IpcError("disconnected"));
    }
    this.pending.clear();
  }

  private handleMessage(raw: unknown): void {
    if (typeof raw !== "string") return;
    let json: unknown;
    try {
      json = JSON.parse(raw);
    } catch {
      return;
    }
    const parsed = serverMessageSchema.safeParse(json);
    if (!parsed.success) return;
    const message = parsed.data;
    switch (message.type) {
      case "state":
        // 整体替换快照：UI 一切以 core 广播为准
        useStore.setState({ connected: true, snapshot: message.snapshot });
        break;
      case "preview": {
        const snapshot = useStore.getState().snapshot;
        if (snapshot) {
          useStore.setState({ snapshot: { ...snapshot, preview: message.preview } });
        }
        break;
      }
      case "result": {
        const entry = this.pending.get(message.id);
        if (!entry) return;
        this.pending.delete(message.id);
        clearTimeout(entry.timer);
        if (message.ok) entry.resolve();
        else entry.reject(new IpcError(message.error ?? "internal"));
        break;
      }
    }
  }
}

/** 单例。 */
export const wsClient = new WsClient();
