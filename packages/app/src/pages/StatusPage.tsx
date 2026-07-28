import { useEffect, useState } from "react";
import { Badge, Card, KvRow } from "../components";
import {
  mediaProviderKindText,
  relativeTime,
  runtimeStateText,
  runtimeStateTone,
} from "../labels";
import { useStore } from "../store";

export function StatusPage() {
  const snapshot = useStore((s) => s.snapshot);
  const connected = useStore((s) => s.connected);
  const [, setTick] = useState(0);

  // 定时重渲染以刷新相对时间
  useEffect(() => {
    const timer = setInterval(() => setTick((n) => n + 1), 10_000);
    return () => clearInterval(timer);
  }, []);

  if (!snapshot) return null;
  const provider = snapshot.mediaProvider;

  return (
    <div className="page">
      <h1 className="page-title">状态</h1>

      <Card title="运行状态">
        <KvRow label="Live Desk">
          <Badge tone={runtimeStateTone(snapshot.runtimeState)}>
            {runtimeStateText[snapshot.runtimeState]}
          </Badge>
        </KvRow>
        <KvRow label="核心服务连接">
          <Badge tone={connected ? "ok" : "err"}>{connected ? "已连接" : "未连接"}</Badge>
        </KvRow>
        <KvRow label="最近发布">
          {snapshot.lastPublishAt !== null ? relativeTime(snapshot.lastPublishAt) : "从未"}
        </KvRow>
        <KvRow label="最近错误">
          {snapshot.lastError !== null ? (
            <span className="error-text">{snapshot.lastError}</span>
          ) : (
            "无"
          )}
        </KvRow>
      </Card>

      <Card title="组件">
        <KvRow label="媒体提供者">
          {mediaProviderKindText[provider.kind]}{" "}
          <Badge tone={provider.healthy ? "ok" : "err"}>{provider.healthy ? "正常" : "异常"}</Badge>
        </KvRow>
        {provider.detail ? <p className="hint">{provider.detail}</p> : null}
        <KvRow label="核心版本">
          <span className="mono">{snapshot.version}</span>
        </KvRow>
      </Card>
    </div>
  );
}
