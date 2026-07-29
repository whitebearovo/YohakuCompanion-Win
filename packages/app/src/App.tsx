import type { CSSProperties, ReactNode } from "react";
import { AboutPage } from "./pages/AboutPage";
import { GeneralPage } from "./pages/GeneralPage";
import { PrivacyRulesPage } from "./pages/PrivacyRulesPage";
import { StatusPage } from "./pages/StatusPage";
import { YohakuPage } from "./pages/YohakuPage";
import { useStore, type PageId } from "./store";
import { useBackgroundSettings } from "./background";

const NAV_ITEMS: Array<{ id: PageId; label: string }> = [
  { id: "general", label: "通用" },
  { id: "yohaku", label: "Yohaku" },
  { id: "privacy", label: "隐私规则" },
  { id: "status", label: "状态" },
  { id: "about", label: "关于" },
];

export function App() {
  const connected = useStore((s) => s.connected);
  const snapshot = useStore((s) => s.snapshot);
  const coreDead = useStore((s) => s.coreDead);
  const page = useStore((s) => s.page);
  const setPage = useStore((s) => s.setPage);
  const background = useBackgroundSettings();

  const ready = connected && snapshot !== null;

  let content: ReactNode = null;
  if (ready) {
    switch (page) {
      case "general":
        content = <GeneralPage />;
        break;
      case "yohaku":
        content = <YohakuPage />;
        break;
      case "privacy":
        content = <PrivacyRulesPage />;
        break;
      case "status":
        content = <StatusPage />;
        break;
      case "about":
        content = <AboutPage />;
        break;
    }
  }

  return (
    <div
      className="app"
      style={
        {
          "--app-background-image": background.dataUrl ? `url(${background.dataUrl})` : "none",
          "--app-background-blur": `${background.blur}px`,
          "--app-background-opacity": `${background.opacity / 100}`,
        } as CSSProperties
      }
    >
      {coreDead ? (
        <div className="error-bar">核心服务已停止且无法自动恢复，请退出后重新启动应用。</div>
      ) : null}
      <div className="layout">
        <nav className="nav">
          <div className="nav-brand">Yohaku Companion</div>
          {NAV_ITEMS.map((item) => (
            <button
              key={item.id}
              type="button"
              className={item.id === page ? "nav-item active" : "nav-item"}
              onClick={() => setPage(item.id)}
            >
              {item.label}
            </button>
          ))}
        </nav>
        <main className="content">
          <div key={page} className="page-transition">
            {content}
          </div>
        </main>
      </div>
      {!ready && !coreDead ? (
        <div className="overlay">
          <div className="overlay-box">
            <div className="spinner" />
            <div>正在连接核心服务…</div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
