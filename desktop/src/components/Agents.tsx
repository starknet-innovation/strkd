import { useEffect, useState } from "react";
import { api, type ClientInfo } from "../api";

/// Per-agent permission control panel. Grant a time-bounded auto-approval window
/// (1–3 months) so an agent can transact with its own accounts without a prompt;
/// funding always still prompts. Revocable any time.
export function Agents() {
  const [clients, setClients] = useState<ClientInfo[]>([]);
  const [days, setDays] = useState(30);
  const [err, setErr] = useState("");

  const load = () => api.listClients().then(setClients).catch(() => {});
  useEffect(() => {
    load();
    const id = setInterval(load, 3000);
    return () => clearInterval(id);
  }, []);

  function activeRemaining(until: number | null): string | null {
    if (!until) return null;
    const ms = until - Date.now();
    if (ms <= 0) return null;
    const d = Math.floor(ms / 86_400_000);
    const h = Math.floor((ms % 86_400_000) / 3_600_000);
    return d > 0 ? `${d}d ${h}h left` : `${h}h left`;
  }

  async function grant(id: string) {
    setErr("");
    try {
      await api.grantPermission(id, days);
      load();
    } catch (e) {
      setErr(String(e));
    }
  }
  async function revoke(id: string) {
    setErr("");
    try {
      await api.revokePermission(id);
      load();
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <div className="panel">
      <p className="muted small">
        Granted agents can transact with <strong>their own</strong> accounts without approval for
        the chosen window. Funding always requires your approval. Revoke any time.
      </p>

      <div className="addrow">
        <label className="muted small" style={{ display: "flex", alignItems: "center", gap: 6 }}>
          Grant length
          <select className="input" value={days} onChange={(e) => setDays(Number(e.target.value))}>
            <option value={30}>1 month</option>
            <option value={60}>2 months</option>
            <option value={90}>3 months</option>
          </select>
        </label>
      </div>

      {err && <p className="error">{err}</p>}

      <ul className="list">
        {clients.length === 0 && <li className="muted">No paired clients yet.</li>}
        {clients.map((c) => {
          const remaining = activeRemaining(c.granted_until);
          return (
            <li key={c.id} className="acct">
              <div className="acct-main">
                <span className="acct-label">{c.label}</span>
                <span className={`badge ${c.kind}`}>{c.kind}</span>
                {remaining ? (
                  <span className="badge ok">granted · {remaining}</span>
                ) : (
                  <span className="badge">approval required</span>
                )}
                <span className="spacer" />
                {remaining ? (
                  <button className="ghost small-btn" onClick={() => revoke(c.id)}>
                    Revoke
                  </button>
                ) : (
                  <button className="primary small-btn" onClick={() => grant(c.id)}>
                    Grant
                  </button>
                )}
              </div>
              <code className="addr">{c.id}</code>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
