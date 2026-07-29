import { useSyncExternalStore } from "react";
import { Card } from "../components";
import { useStore } from "../store";
import {
  checkAndInstallUpdate,
  getUpdaterError,
  getUpdaterProgress,
  getUpdaterStatus,
  getUpdateSource,
  setUpdateSource,
  subscribeUpdater,
} from "../updater";

const REPOSITORY_URL = "https://github.com/whitebearovo/YohakuCompanion-Win";

const STATUS_LABELS = {
  idle: "尚未检查",
  checking: "正在检查更新…",
  downloading: "正在下载并安装更新…",
  installed: "更新已安装，正在重启…",
  upToDate: "已是最新版本",
  error: "检查更新失败",
} as const;

export function AboutPage() {
  const snapshot = useStore((state) => state.snapshot);
  const updaterStatus = useSyncExternalStore(
    subscribeUpdater,
    getUpdaterStatus,
    getUpdaterStatus,
  );
  const updaterError = getUpdaterError();
  const updaterProgress = getUpdaterProgress();
  const updateSource = getUpdateSource();
  const checking = updaterStatus === "checking" || updaterStatus === "downloading";
  const progressPercent =
    updaterProgress?.total && updaterProgress.total > 0
      ? Math.min(100, Math.round((updaterProgress.downloaded / updaterProgress.total) * 100))
      : null;

  return (
    <div className="page about-page">
      <div className="about-heading">
        <div className="about-mark" aria-hidden="true">Y</div>
        <div>
          <h1 className="page-title">关于 Yohaku Companion</h1>
          <p className="hint">连接桌面状态与 Yohaku Live Desk。</p>
        </div>
      </div>

      <Card title="应用信息">
        <div className="about-info-grid">
          <div className="about-info-item">
            <span className="about-label">版本</span>
            <strong className="mono">{snapshot?.version ?? "未知"}</strong>
          </div>
          <div className="about-info-item">
            <span className="about-label">开发者</span>
            <strong>whitebearovo</strong>
          </div>
        </div>
      </Card>

      <Card title="软件更新">
        <p className="hint">应用会自动检查更新，也可以随时手动检查。</p>
        <label className="update-source-field">
          <span className="field-label">更新源</span>
          <select
            className="select"
            value={updateSource}
            disabled={checking}
            onChange={(event) => setUpdateSource(event.target.value as "github" | "mirror")}
          >
            <option value="github">GitHub 官方源</option>
            <option value="mirror">中国大陆镜像源</option>
          </select>
        </label>
        <div className="about-update-row">
          <span className={`update-status update-status-${updaterStatus}`}>
            <span className="update-status-dot" aria-hidden="true" />
            {STATUS_LABELS[updaterStatus]}
          </span>
          <button
            type="button"
            className="btn primary"
            disabled={checking}
            onClick={() => void checkAndInstallUpdate(updateSource).catch(() => undefined)}
          >
            {checking ? "请稍候" : "检查更新"}
          </button>
        </div>
        {updaterStatus === "downloading" && updaterProgress ? (
          <div className="update-progress" aria-label="更新下载进度">
            <div className="update-progress-head">
              <span>{updaterProgress.version ? `正在下载 v${updaterProgress.version}` : "正在下载更新"}</span>
              <span>{progressPercent === null ? "准备中" : `${progressPercent}%`}</span>
            </div>
            <progress value={progressPercent ?? undefined} max="100" />
          </div>
        ) : null}
        {updaterStatus === "error" && updaterError ? (
          <p className="form-error about-error">{updaterError}</p>
        ) : null}
      </Card>

      <Card title="项目">
        <p className="hint">源代码、问题反馈和版本发布信息：</p>
        <a className="repository-link" href={REPOSITORY_URL} target="_blank" rel="noreferrer">
          {REPOSITORY_URL}
        </a>
      </Card>
    </div>
  );
}
