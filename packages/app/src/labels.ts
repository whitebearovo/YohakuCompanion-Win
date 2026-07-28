import type { MediaProviderHealth, RuntimeState } from "@yohaku/shared";
import { IpcError, type SendErrorCode } from "./wsClient";

/** runtimeState → 中文标签（托盘状态行 / 徽章共用）。 */
export const runtimeStateText: Record<RuntimeState, string> = {
  notPaired: "未配对",
  disabled: "已暂停",
  connecting: "连接中",
  active: "分享中",
  degraded: "已降级",
  suspended: "已挂起",
  updateRequired: "需要更新客户端",
  serverFeatureUnavailable: "服务器不支持 Live Desk",
};

export type Tone = "ok" | "warn" | "err" | "muted";

export function runtimeStateTone(state: RuntimeState): Tone {
  switch (state) {
    case "active":
      return "ok";
    case "degraded":
    case "suspended":
      return "warn";
    case "updateRequired":
    case "serverFeatureUnavailable":
      return "err";
    case "notPaired":
    case "disabled":
    case "connecting":
      return "muted";
  }
}

/** IPC 错误码 → 中文文案。 */
export const ipcErrorText: Record<SendErrorCode, string> = {
  previewOutOfDate: "预览已更新，请重新确认",
  notPaired: "尚未配对",
  alreadyPaired: "本机已配对，请先解除配对",
  pairingExpired: "配对码已过期或无效",
  pairingFailed: "配对失败，请检查服务器地址与配对码",
  requiredScopeMissing: "服务器未授予 presence 权限",
  clientUpdateRequired: "客户端版本过旧，请更新后重试",
  serverFeatureUnavailable: "服务器不支持 Live Desk",
  rateLimited: "请求过于频繁，请稍后再试",
  validationFailed: "数据校验失败",
  credentialStoreUnavailable: "本机凭证存储不可用",
  network: "无法连接服务器",
  invalidInput: "输入无效",
  internal: "内部错误",
  timeout: "请求超时",
  disconnected: "未连接核心服务",
};

export function sendErrorMessage(error: unknown): string {
  if (error instanceof IpcError) return ipcErrorText[error.code];
  return "未知错误";
}

export const mediaProviderKindText: Record<MediaProviderHealth["kind"], string> = {
  npm: "Node 原生媒体模块",
  powershell: "PowerShell 回退方案",
  none: "不可用",
};

/** 相对时间（毫秒时间戳）。 */
export function relativeTime(ts: number): string {
  const diffMs = Date.now() - ts;
  if (diffMs < 5_000) return "刚刚";
  const seconds = Math.floor(diffMs / 1000);
  if (seconds < 60) return `${seconds} 秒前`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  return new Date(ts).toLocaleString();
}
