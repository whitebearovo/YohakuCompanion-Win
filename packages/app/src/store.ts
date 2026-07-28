import { create } from "zustand";
import type { CoreStateSnapshot } from "@yohaku/shared";

export type PageId = "general" | "yohaku" | "privacy" | "status";

/**
 * 单一 zustand store。业务数据只有 `snapshot` 一份，整体来自 core 的 state
 * 广播；UI 绝不保留业务状态副本。其余字段均为纯 UI 状态。
 */
interface AppStore {
  /** WebSocket 是否已连上核心服务 */
  connected: boolean;
  /** core 广播的完整状态快照（唯一业务数据来源） */
  snapshot: CoreStateSnapshot | null;
  /** shell 报告 core 已崩溃且重启次数用尽（"core-dead" 事件） */
  coreDead: boolean;
  /** 当前页面（纯 UI 导航状态，托盘"恢复分享"也会设置它） */
  page: PageId;
  setPage: (page: PageId) => void;
}

export const useStore = create<AppStore>()((set) => ({
  connected: false,
  snapshot: null,
  coreDead: false,
  page: "yohaku",
  setPage: (page) => set({ page }),
}));
