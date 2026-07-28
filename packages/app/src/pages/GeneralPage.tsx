import { useEffect, useState } from "react";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import { Badge, Card, KvRow, Toggle } from "../components";
import { mediaProviderKindText } from "../labels";
import { useStore } from "../store";
import { wsClient, type CommandInput } from "../wsClient";

function run(cmd: CommandInput): void {
  // 命令成功后 core 必然广播新 state，UI 随快照回流刷新；失败则快照不变、开关自动回弹
  void wsClient.send(cmd).catch((error) => console.error("[general] command failed", error));
}

export function GeneralPage() {
  const snapshot = useStore((s) => s.snapshot);
  const [autostartOn, setAutostartOn] = useState<boolean | null>(null);

  useEffect(() => {
    let alive = true;
    isEnabled()
      .then((value) => {
        if (alive) setAutostartOn(value);
      })
      .catch(() => {
        if (alive) setAutostartOn(false);
      });
    return () => {
      alive = false;
    };
  }, []);

  if (!snapshot) return null;
  const privacy = snapshot.privacy;
  const provider = snapshot.mediaProvider;

  const toggleAutostart = (next: boolean): void => {
    setAutostartOn(next);
    void (next ? enable() : disable())
      .catch((error) => console.error("[general] autostart failed", error))
      .then(() => isEnabled())
      .then((value) => setAutostartOn(value))
      .catch(() => setAutostartOn(next));
  };

  return (
    <div className="page">
      <h1 className="page-title">通用</h1>

      <Card title="采集源">
        <Toggle
          checked={privacy.sources.application}
          onChange={(v) => run({ cmd: "setSources", application: v })}
          label="前台应用"
          desc="采集当前前台应用及其窗口，用于展示你正在使用的软件。"
        />
        <Toggle
          checked={privacy.sources.media}
          onChange={(v) => run({ cmd: "setSources", media: v })}
          label="媒体播放"
          desc="采集系统媒体会话（音乐、播客、视频）的播放信息。"
        />
      </Card>

      <Card title="隐私">
        <Toggle
          checked={privacy.shareWindowTitles}
          onChange={(v) => run({ cmd: "setPrivacy", patch: { shareWindowTitles: v } })}
          label="分享窗口标题"
          desc="全局开关：关闭时任何应用的窗口标题都不会被分享。"
        />
        <Toggle
          checked={privacy.ignoreNullArtist}
          onChange={(v) => run({ cmd: "setPrivacy", patch: { ignoreNullArtist: v } })}
          label="忽略无歌手信息的媒体"
          desc="媒体没有歌手（artist）字段时视为噪音，不进行分享。"
        />
      </Card>

      <Card title="启动">
        <Toggle
          checked={autostartOn ?? false}
          disabled={autostartOn === null}
          onChange={toggleAutostart}
          label="开机自启动"
          desc="登录 Windows 后自动在后台启动 Yohaku Companion。"
        />
      </Card>

      <Card title="媒体提供者">
        <KvRow label="当前方案">{mediaProviderKindText[provider.kind]}</KvRow>
        <KvRow label="健康状态">
          <Badge tone={provider.healthy ? "ok" : "err"}>{provider.healthy ? "正常" : "异常"}</Badge>
        </KvRow>
        {provider.detail ? <p className="hint">{provider.detail}</p> : null}
      </Card>
    </div>
  );
}
