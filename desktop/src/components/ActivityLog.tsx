import { useEffect, useState } from "react";
import { api, type LogEntry } from "../api";

export function ActivityLog() {
  const [log, setLog] = useState<LogEntry[]>([]);

  useEffect(() => {
    const tick = () => api.recentLog(100).then(setLog).catch(() => {});
    tick();
    const id = setInterval(tick, 2000);
    return () => clearInterval(id);
  }, []);

  return (
    <div className="panel">
      <ul className="list">
        {log.length === 0 && <li className="muted">No requests yet.</li>}
        {log.map((e, i) => (
          <li key={i} className="logentry">
            <div className="log-main">
              <span className="log-method">{e.method}</span>
              <span className={`badge ${e.outcome === "ok" ? "ok" : "err"}`}>{e.decision}</span>
            </div>
            <span className="muted small">
              {e.client ?? "—"} · {e.outcome}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}
