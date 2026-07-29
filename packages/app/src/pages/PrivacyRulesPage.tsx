import { useEffect, useState } from "react";
import type {
  ApplicationPrivacyRule,
  PrivacyDefault,
  PrivacyMapping,
  PrivacyOverride,
} from "@yohaku/shared";
import { Card } from "../components";
import { sendErrorMessage } from "../labels";
import { useStore } from "../store";
import { wsClient } from "../wsClient";

const OVERRIDE_OPTIONS: Array<{ value: PrivacyOverride; label: string }> = [
  { value: "inherit", label: "继承默认" },
  { value: "share", label: "分享" },
  { value: "hide", label: "隐藏" },
];

export function PrivacyRulesPage() {
  const snapshot = useStore((s) => s.snapshot);
  const [newAppId, setNewAppId] = useState("");
  const [error, setError] = useState<string | null>(null);

  if (!snapshot) return null;
  const privacy = snapshot.privacy;

  const patchDefaults = (defaults: {
    application?: PrivacyDefault;
    windowTitle?: PrivacyDefault;
    media?: PrivacyDefault;
  }): void => {
    setError(null);
    void wsClient
      .send({ cmd: "setPrivacy", patch: { defaults } })
      .catch((e) => setError(sendErrorMessage(e)));
  };

  const addRule = (rawId: string): void => {
    const appId = rawId.trim().toLowerCase();
    if (!appId) return;
    setError(null);
    void wsClient
      .send({
        cmd: "upsertRule",
        rule: { appId, application: "inherit", windowTitle: "inherit", media: "inherit" },
      })
      .catch((e) => setError(sendErrorMessage(e)));
    setNewAppId("");
  };

  const recentCandidates = snapshot.recentAppIds.filter(
    (id) => !privacy.rules.some((rule) => rule.appId === id),
  );

  return (
    <div className="page">
      <h1 className="page-title">隐私规则</h1>
      {error ? <div className="form-error">{error}</div> : null}

      <Card title="全局默认">
        <p className="hint">没有专门规则的应用按以下默认策略处理。</p>
        <DefaultPicker
          label="应用"
          value={privacy.defaults.application}
          onChange={(v) => patchDefaults({ application: v })}
        />
        <DefaultPicker
          label="窗口标题"
          value={privacy.defaults.windowTitle}
          onChange={(v) => patchDefaults({ windowTitle: v })}
        />
        <DefaultPicker
          label="媒体"
          value={privacy.defaults.media}
          onChange={(v) => patchDefaults({ media: v })}
        />
      </Card>

      <Card title="应用规则">
        <p className="hint">
          按进程可执行文件名（小写，如 code.exe）匹配；无法映射到进程的媒体会话按播放器名匹配。
          修改立即生效。
        </p>
        {privacy.rules.length === 0 ? (
          <p className="muted">暂无规则。</p>
        ) : (
          <table className="rules-table">
            <thead>
              <tr>
                <th>appId</th>
                <th>应用</th>
                <th>窗口标题</th>
                <th>媒体</th>
                <th>别名</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {privacy.rules.map((rule) => (
                <RuleRow key={rule.appId} rule={rule} onError={setError} />
              ))}
            </tbody>
          </table>
        )}
        <div className="add-rule">
          <input
            className="input"
            value={newAppId}
            placeholder="appId（如 code.exe）"
            onChange={(e) => setNewAppId(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") addRule(newAppId);
            }}
          />
          <button type="button" className="btn" onClick={() => addRule(newAppId)}>
            添加规则
          </button>
        </div>
        {recentCandidates.length > 0 ? (
          <div className="chips">
            <span className="hint">最近的应用：</span>
            {recentCandidates.map((id) => (
              <button key={id} type="button" className="chip" onClick={() => addRule(id)}>
                {id}
              </button>
            ))}
          </div>
        ) : null}
      </Card>

      <MappingsEditor mappings={privacy.mappings} onError={setError} />
    </div>
  );
}

function DefaultPicker(props: {
  label: string;
  value: PrivacyDefault;
  onChange: (value: PrivacyDefault) => void;
}) {
  return (
    <div className="default-row">
      <span className="default-label">{props.label}</span>
      <div className="seg">
        <button
          type="button"
          className={props.value === "share" ? "seg-btn active" : "seg-btn"}
          onClick={() => props.onChange("share")}
        >
          分享
        </button>
        <button
          type="button"
          className={props.value === "hide" ? "seg-btn active" : "seg-btn"}
          onClick={() => props.onChange("hide")}
        >
          隐藏
        </button>
      </div>
    </div>
  );
}

function OverrideSelect(props: { value: PrivacyOverride; onChange: (v: PrivacyOverride) => void }) {
  return (
    <select
      className="select"
      value={props.value}
      onChange={(e) => props.onChange(e.target.value as PrivacyOverride)}
    >
      {OVERRIDE_OPTIONS.map((option) => (
        <option key={option.value} value={option.value}>
          {option.label}
        </option>
      ))}
    </select>
  );
}

function RuleRow(props: { rule: ApplicationPrivacyRule; onError: (msg: string) => void }) {
  const { rule, onError } = props;
  const [alias, setAlias] = useState(rule.displayAlias ?? "");

  // core 广播的新值到达时同步本地草稿
  useEffect(() => {
    setAlias(rule.displayAlias ?? "");
  }, [rule.displayAlias]);

  const upsert = (patch: {
    application?: PrivacyOverride;
    windowTitle?: PrivacyOverride;
    media?: PrivacyOverride;
    alias?: string;
  }): void => {
    const nextAlias = (patch.alias ?? alias).trim();
    const next: ApplicationPrivacyRule = {
      appId: rule.appId,
      application: patch.application ?? rule.application,
      windowTitle: patch.windowTitle ?? rule.windowTitle,
      media: patch.media ?? rule.media,
    };
    if (nextAlias) next.displayAlias = nextAlias;
    void wsClient.send({ cmd: "upsertRule", rule: next }).catch((e) => onError(sendErrorMessage(e)));
  };

  const commitAlias = (): void => {
    if (alias.trim() === (rule.displayAlias ?? "")) return;
    upsert({ alias });
  };

  const remove = (): void => {
    void wsClient
      .send({ cmd: "deleteRule", appId: rule.appId })
      .catch((e) => onError(sendErrorMessage(e)));
  };

  return (
    <tr>
      <td className="mono">{rule.appId}</td>
      <td>
        <OverrideSelect value={rule.application} onChange={(v) => upsert({ application: v })} />
      </td>
      <td>
        <OverrideSelect value={rule.windowTitle} onChange={(v) => upsert({ windowTitle: v })} />
      </td>
      <td>
        <OverrideSelect value={rule.media} onChange={(v) => upsert({ media: v })} />
      </td>
      <td>
        <input
          className="input alias-input"
          value={alias}
          placeholder="显示别名"
          onChange={(e) => setAlias(e.target.value)}
          onBlur={commitAlias}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
          }}
        />
      </td>
      <td>
        <button type="button" className="btn ghost small" onClick={remove}>
          删除
        </button>
      </td>
    </tr>
  );
}

function MappingsEditor(props: { mappings: PrivacyMapping[]; onError: (msg: string) => void }) {
  const { mappings, onError } = props;
  const [draft, setDraft] = useState<PrivacyMapping[]>(mappings);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);

  // 没有本地未保存修改时，跟随 core 广播刷新
  useEffect(() => {
    if (!dirty) setDraft(mappings);
  }, [mappings, dirty]);

  const update = (index: number, patch: Partial<PrivacyMapping>): void => {
    setDraft((rows) => rows.map((row, i) => (i === index ? { ...row, ...patch } : row)));
    setDirty(true);
  };

  const addRow = (): void => {
    setDraft((rows) => [...rows, { type: "process_name", from: "", to: "" }]);
    setDirty(true);
  };

  const removeRow = (index: number): void => {
    setDraft((rows) => rows.filter((_, i) => i !== index));
    setDirty(true);
  };

  const save = (): void => {
    const cleaned: PrivacyMapping[] = [];
    for (const row of draft) {
      const from = row.from.trim();
      const to = row.to.trim();
      if (!from && !to) continue; // 整行为空视为放弃该行
      if (!from || !to) {
        onError("映射的 from 与 to 都不能为空");
        return;
      }
      cleaned.push({ type: row.type, from, to });
    }
    setSaving(true);
    void wsClient
      .send({ cmd: "setMappings", mappings: cleaned })
      .then(() => setDirty(false))
      .catch((e) => onError(sendErrorMessage(e)))
      .finally(() => setSaving(false));
  };

  const discard = (): void => {
    setDraft(mappings);
    setDirty(false);
  };

  return (
    <Card title="应用名替换">
      <p className="hint">
        按进程名替换上报的应用显示名，例如将 code.exe 替换为 Visual Studio Code。
        process_name 用于前台应用；media_player_name 匹配读取到的播放器名称。未配置媒体播放器映射时，使用读取到的原名称。修改后需点击保存。
      </p>
      {draft.length === 0 ? (
        <p className="muted">暂无映射。</p>
      ) : (
        <table className="rules-table">
          <thead>
            <tr>
              <th>类型</th>
              <th>进程名</th>
              <th>上报名称</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {draft.map((row, index) => (
              <tr key={index}>
                <td>
                  <select
                    className="select"
                    value={row.type}
                    onChange={(e) =>
                      update(index, { type: e.target.value as PrivacyMapping["type"] })
                    }
                  >
                    <option value="process_name">进程名</option>
                    <option value="media_player_name">媒体播放器名称</option>
                    <option value="media_process_name">旧版媒体名称</option>
                  </select>
                </td>
                <td>
                  <input
                    className="input"
                    value={row.from}
                    placeholder="code.exe"
                    onChange={(e) => update(index, { from: e.target.value })}
                  />
                </td>
                <td>
                  <input
                    className="input"
                    value={row.to}
                    placeholder="Visual Studio Code"
                    onChange={(e) => update(index, { to: e.target.value })}
                  />
                </td>
                <td>
                  <button type="button" className="btn ghost small" onClick={() => removeRow(index)}>
                    删除
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <div className="actions">
        <button type="button" className="btn" onClick={addRow}>
          添加映射
        </button>
        {dirty ? (
          <>
            <button type="button" className="btn primary" disabled={saving} onClick={save}>
              {saving ? "保存中…" : "保存映射"}
            </button>
            <button type="button" className="btn ghost" disabled={saving} onClick={discard}>
              放弃更改
            </button>
            <span className="hint">有未保存的更改</span>
          </>
        ) : null}
      </div>
    </Card>
  );
}
