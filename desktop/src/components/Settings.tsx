import { useEffect, useState } from "react";
import { api, type Settings as SettingsT } from "../api";

/// Control panel for the Starknet RPC endpoints that power broadcasting + fee
/// estimation. Changes take effect immediately (no restart).
export function Settings({ onChange }: { onChange: () => void }) {
  const [settings, setSettings] = useState<SettingsT>({
    sepolia_rpc: "",
    mainnet_rpc: "",
    auto_lock_minutes: 15,
  });
  const [saved, setSaved] = useState(false);
  const [err, setErr] = useState("");

  useEffect(() => {
    api.getSettings().then(setSettings).catch(() => {});
  }, []);

  async function save() {
    setErr("");
    setSaved(false);
    try {
      await api.setSettings(settings);
      setSaved(true);
      setTimeout(() => setSaved(false), 1500);
      onChange();
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <div className="panel">
      <h3>Starknet RPC endpoints</h3>
      <p className="muted small">
        Required for fee estimation and broadcasting (<code>submit:true</code>). Leave a field
        empty to keep that network sign-only. The endpoint stays on this device and is never
        exposed over the wallet's service.
      </p>

      <label className="field">
        <span className="muted small">Sepolia RPC URL</span>
        <input
          className="input"
          placeholder="https://…"
          value={settings.sepolia_rpc}
          onChange={(e) => setSettings({ ...settings, sepolia_rpc: e.target.value })}
        />
      </label>

      <label className="field">
        <span className="muted small">Mainnet RPC URL</span>
        <input
          className="input"
          placeholder="https://…"
          value={settings.mainnet_rpc}
          onChange={(e) => setSettings({ ...settings, mainnet_rpc: e.target.value })}
        />
      </label>

      <h3>Security</h3>
      <label className="field">
        <span className="muted small">Auto-lock after (minutes of inactivity; 0 = never)</span>
        <input
          className="input"
          type="number"
          min={0}
          value={settings.auto_lock_minutes}
          onChange={(e) =>
            setSettings({ ...settings, auto_lock_minutes: Math.max(0, Number(e.target.value) || 0) })
          }
        />
      </label>

      <button className="primary" onClick={save}>
        {saved ? "Saved ✓" : "Save"}
      </button>
      {err && <p className="error">{err}</p>}
    </div>
  );
}
