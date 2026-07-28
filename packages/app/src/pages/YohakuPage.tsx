import { useEffect, useState } from "react";
import type { ConnectionSummary, CoreStateSnapshot } from "@yohaku/shared";
import { Badge, Card, KvRow } from "../components";
import { runtimeStateText, runtimeStateTone, sendErrorMessage } from "../labels";
import { useStore } from "../store";
import { IpcError, wsClient } from "../wsClient";

export function YohakuPage() {
  const snapshot = useStore((s) => s.snapshot);
  if (!snapshot) return null;
  return (
    <div className="page">
      <h1 className="page-title">Yohaku</h1>
      {snapshot.connection === null ? (
        <PairForm />
      ) : (
        <PairedView snapshot={snapshot} connection={snapshot.connection} />
      )}
    </div>
  );
}

function PairForm() {
  const [baseUrl, setBaseUrl] = useState("");
  const [deviceName, setDeviceName] = useState("Windows PC");
  const [pairingCode, setPairingCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (): Promise<void> => {
    setError(null);
    if (!baseUrl.trim() || !deviceName.trim() || !pairingCode.trim()) {
      setError("请填写完整信息");
      return;
    }
    setBusy(true);
    try {
      await wsClient.send({
        cmd: "pair",
        baseUrl: baseUrl.trim(),
        deviceName: deviceName.trim(),
        pairingCode: pairingCode.trim(),
      });
      // 成功后 core 会广播带 connection 的新快照，本表单随之卸载
    } catch (e) {
      setError(sendErrorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card title="配对到 Yohaku 服务器">
      <p className="hint">
        在 Mix Space Admin 的 Companion 页面生成一次性配对码。配对只安装可撤销的设备凭证，
        不会立即开始分享任何内容。
      </p>
      <form
        className="pair-form"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <label className="field">
          <span className="field-label">服务器 URL</span>
          <input
            className="input"
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            placeholder="https://your-site.example"
            autoFocus
          />
        </label>
        <label className="field">
          <span className="field-label">设备名</span>
          <input
            className="input"
            value={deviceName}
            onChange={(e) => setDeviceName(e.target.value)}
          />
        </label>
        <label className="field">
          <span className="field-label">配对码</span>
          <input
            className="input"
            value={pairingCode}
            onChange={(e) => setPairingCode(e.target.value)}
            placeholder="10 分钟内有效，只能使用一次"
          />
        </label>
        {error ? <div className="form-error">{error}</div> : null}
        <div className="actions">
          <button className="btn primary" type="submit" disabled={busy}>
            {busy ? "配对中…" : "配对"}
          </button>
        </div>
      </form>
    </Card>
  );
}

function PairedView(props: { snapshot: CoreStateSnapshot; connection: ConnectionSummary }) {
  const { snapshot, connection } = props;
  const [busy, setBusy] = useState(false);
  const [stale, setStale] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 进入页面立即请求预览，之后每 3 秒刷新（仅页面可见时）
  useEffect(() => {
    const request = (): void => {
      if (document.hidden) return;
      void wsClient.send({ cmd: "requestPreview" }).catch(() => undefined);
    };
    request();
    const timer = setInterval(request, 3000);
    return () => clearInterval(timer);
  }, []);

  const preview = snapshot.preview;

  const confirmLiveDesk = async (): Promise<void> => {
    if (!preview) return;
    setBusy(true);
    setError(null);
    try {
      await wsClient.send({
        cmd: "confirmConsent",
        policyFingerprint: preview.policyFingerprint,
      });
      setStale(false);
    } catch (e) {
      if (e instanceof IpcError && e.code === "previewOutOfDate") {
        // fail-closed 重确认循环：内容已变化，拉取新预览并要求再次确认
        setStale(true);
        void wsClient.send({ cmd: "requestPreview" }).catch(() => undefined);
      } else {
        setError(sendErrorMessage(e));
      }
    } finally {
      setBusy(false);
    }
  };

  const pauseSharing = (): void => {
    setError(null);
    void wsClient.send({ cmd: "disableLiveDesk" }).catch((e) => setError(sendErrorMessage(e)));
  };

  const unpair = (): void => {
    if (!window.confirm("确定要解除配对吗？本机的设备凭证将被撤销，Live Desk 将停止工作。")) {
      return;
    }
    setError(null);
    void wsClient.send({ cmd: "unpair" }).catch((e) => setError(sendErrorMessage(e)));
  };

  let host = connection.baseUrl;
  try {
    host = new URL(connection.baseUrl).host;
  } catch {
    // 保留原始值
  }
  const deviceIdShort =
    connection.deviceId.length > 10 ? `${connection.deviceId.slice(0, 10)}…` : connection.deviceId;

  const app = preview?.projection.application ?? null;
  const media = preview?.projection.media ?? null;

  return (
    <>
      <Card title="连接">
        <KvRow label="服务器">{host}</KvRow>
        <KvRow label="设备名">{connection.deviceName}</KvRow>
        <KvRow label="设备 ID">
          <span className="mono">{deviceIdShort}</span>
        </KvRow>
        <KvRow label="状态">
          <Badge tone={runtimeStateTone(snapshot.runtimeState)}>
            {runtimeStateText[snapshot.runtimeState]}
          </Badge>
        </KvRow>
      </Card>

      <Card title="净化预览" className={stale ? "preview-stale" : ""}>
        <p className="hint">以下是经过隐私规则净化后、启用分享时将要公开的确切内容。</p>
        {stale ? (
          <div className="notice">
            预览已更新，请重新确认。内容发生变化后必须再次同意，这是隐私保护的正常流程。
          </div>
        ) : null}
        {preview ? (
          <>
            <div className="preview-grid">
              <div className="preview-section">
                <div className="preview-heading">前台应用</div>
                {app ? (
                  <>
                    <div className="preview-line">{app.displayName}</div>
                    <div className="preview-line muted">窗口标题：{app.windowTitle ?? "不分享"}</div>
                    {app.windowTitle === null && !snapshot.privacy.shareWindowTitles ? (
                      <div className="hint">窗口标题未开启分享</div>
                    ) : null}
                  </>
                ) : (
                  <div className="muted">不分享</div>
                )}
              </div>
              <div className="preview-section">
                <div className="preview-heading">媒体</div>
                {media ? (
                  <>
                    <div className="preview-line">
                      {media.title ?? "（无标题）"}
                      {media.artist ? ` — ${media.artist}` : ""}
                    </div>
                    <div className="preview-line muted">
                      {media.playback.state === "playing" ? "播放中" : "已暂停"}
                      {media.playerDisplayName ? ` · ${media.playerDisplayName}` : ""}
                    </div>
                  </>
                ) : (
                  <div className="muted">不分享</div>
                )}
              </div>
            </div>
            <div className="preview-meta">采集于 {new Date(preview.observedAt).toLocaleTimeString()}</div>
          </>
        ) : (
          <div className="muted">正在获取预览…</div>
        )}
        {error ? <div className="form-error">{error}</div> : null}
        <div className="actions">
          {connection.liveDeskEnabled ? (
            <button type="button" className="btn" onClick={pauseSharing}>
              暂停分享
            </button>
          ) : (
            <button
              type="button"
              className="btn primary"
              disabled={busy || !preview}
              onClick={() => void confirmLiveDesk()}
            >
              启用 Live Desk（我确认以上内容将公开）
            </button>
          )}
        </div>
      </Card>

      <Card title="危险操作" className="danger-zone">
        <p className="hint">解除配对将撤销本机的设备凭证，并清除服务器上的 Live Desk 状态。</p>
        <button type="button" className="btn danger" onClick={unpair}>
          解除配对
        </button>
      </Card>
    </>
  );
}
