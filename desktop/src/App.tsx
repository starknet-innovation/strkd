import { useEffect, useState, useCallback } from "react";
import { api, onApprovalRequest, type Status, type ApprovalRequest } from "./api";
import { Onboarding } from "./components/Onboarding";
import { Unlock } from "./components/Unlock";
import { Accounts } from "./components/Accounts";
import { ActivityLog } from "./components/ActivityLog";
import { Connect } from "./components/Connect";
import { Settings } from "./components/Settings";
import { Agents } from "./components/Agents";
import { Proving } from "./components/Proving";
import { ApprovalDialog } from "./components/ApprovalDialog";

type Tab = "accounts" | "activity" | "agents" | "proving" | "connect" | "settings";

/// Cmd/Ctrl +, -, 0 zoom the whole UI (persisted). Answers "can I make it
/// bigger with ⌘+?" — yes.
function useZoom() {
  useEffect(() => {
    const apply = (z: number) => {
      // `zoom` is supported by the WebKit/Chromium webviews Tauri uses.
      (document.body.style as unknown as { zoom: string }).zoom = String(z);
      localStorage.setItem("zoom", String(z));
    };
    let z = parseFloat(localStorage.getItem("zoom") || "1") || 1;
    apply(z);

    const clamp = (v: number) => Math.min(2, Math.max(0.7, Math.round(v * 100) / 100));
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      if (e.key === "=" || e.key === "+") {
        z = clamp(z + 0.1);
        apply(z);
        e.preventDefault();
      } else if (e.key === "-") {
        z = clamp(z - 0.1);
        apply(z);
        e.preventDefault();
      } else if (e.key === "0") {
        z = 1;
        apply(z);
        e.preventDefault();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}

export default function App() {
  useZoom();
  const [status, setStatus] = useState<Status | null>(null);
  const [tab, setTab] = useState<Tab>("accounts");
  const [approvals, setApprovals] = useState<ApprovalRequest[]>([]);

  const refresh = useCallback(() => {
    api.status().then(setStatus).catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, 2000);
    return () => clearInterval(id);
  }, [refresh]);

  const resolveApproval = useCallback(async (id: number, approved: boolean) => {
    try {
      await api.respondApproval(id, approved);
    } catch {
      /* may have timed out — drop it either way */
    }
    setApprovals((cur) => cur.filter((a) => a.id !== id));
  }, []);

  // Queue incoming approval prompts (shown one at a time) in the in-app dialog.
  // The OS banner + sound + Dock-icon bounce are posted from the Rust side (the
  // approval bridge in src-tauri/src/lib.rs), which stays alive even when this
  // window is hidden in the tray — a webview-posted banner never fired then.
  useEffect(() => {
    const un = onApprovalRequest((req) => {
      setApprovals((cur) => [...cur, req]);
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  const current = approvals[0];

  let view;
  if (!status) {
    view = <div className="loading">connecting…</div>;
  } else if (status.needs_onboarding) {
    view = <Onboarding onDone={refresh} />;
  } else if (status.locked) {
    view = <Unlock onUnlocked={refresh} />;
  } else {
    view = (
      <>
        <nav className="tabs">
          <button className={tab === "accounts" ? "tab active" : "tab"} onClick={() => setTab("accounts")}>
            Accounts
          </button>
          <button className={tab === "activity" ? "tab active" : "tab"} onClick={() => setTab("activity")}>
            Activity
          </button>
          <button className={tab === "agents" ? "tab active" : "tab"} onClick={() => setTab("agents")}>
            Agents
          </button>
          <button className={tab === "proving" ? "tab active" : "tab"} onClick={() => setTab("proving")}>
            Proving
          </button>
          <button className={tab === "connect" ? "tab active" : "tab"} onClick={() => setTab("connect")}>
            Connect
          </button>
          <button className={tab === "settings" ? "tab active" : "tab"} onClick={() => setTab("settings")}>
            Settings
          </button>
        </nav>
        <main className="body">
          {tab === "accounts" && <Accounts status={status} onChange={refresh} />}
          {tab === "activity" && <ActivityLog />}
          {tab === "agents" && <Agents />}
          {tab === "proving" && <Proving />}
          {tab === "connect" && <Connect status={status} />}
          {tab === "settings" && <Settings onChange={refresh} />}
        </main>
      </>
    );
  }

  const unlocked = status && !status.locked && !status.needs_onboarding;

  return (
    <div className="app">
      <header className="topbar">
        <span className="brand">strkd</span>
        <span className="spacer" />
        {status && (
          <span className="muted small">
            {status.network} · {status.locked ? "locked" : `${status.accounts} acct`}
          </span>
        )}
        {unlocked && (
          <button className="lockbtn" onClick={() => api.lock().then(refresh)}>
            Lock
          </button>
        )}
      </header>

      {view}

      {current && (
        <ApprovalDialog
          req={current}
          onApprove={() => resolveApproval(current.id, true)}
          onReject={() => resolveApproval(current.id, false)}
        />
      )}
    </div>
  );
}
