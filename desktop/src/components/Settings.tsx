import { useEffect, useState } from "react";
import {
  api,
  type Settings as SettingsT,
  type ProverSettings,
  type ProverNetworkConfig,
  type StorageStats,
} from "../api";

const EMPTY_PROVER: ProverSettings = {
  mainnet: { rpc_url: "", prover_url: "", prover_api_key: "" },
  testnet: { rpc_url: "", prover_url: "", prover_api_key: "" },
  prover_backend: "",
};

/// Control panel for the Starknet RPC endpoints that power broadcasting + fee
/// estimation, plus the on-device prover's per-network config. Wallet RPC
/// changes take effect immediately; the prover backend choice applies on restart.
export function Settings({ onChange }: { onChange: () => void }) {
  const [settings, setSettings] = useState<SettingsT>({
    sepolia_rpc: "",
    mainnet_rpc: "",
    auto_lock_minutes: 15,
  });
  const [saved, setSaved] = useState(false);
  const [err, setErr] = useState("");

  // Prover settings live in their own store (apply on restart for the backend).
  const [prover, setProver] = useState<ProverSettings>(EMPTY_PROVER);
  const [proverSaved, setProverSaved] = useState(false);
  const [stats, setStats] = useState<StorageStats | null>(null);

  useEffect(() => {
    api.getSettings().then(setSettings).catch(() => {});
    api.getProverSettings().then(setProver).catch(() => {});
    refreshStats();
  }, []);

  function refreshStats() {
    api.storageStats().then(setStats).catch(() => {});
  }

  function proverField(net: "mainnet" | "testnet", key: keyof ProverNetworkConfig, value: string) {
    setProver((s) => ({ ...s, [net]: { ...s[net], [key]: value } }));
    setProverSaved(false);
  }

  async function saveProver() {
    try {
      await api.setProverSettings(prover);
      setProverSaved(true);
      setTimeout(() => setProverSaved(false), 1500);
    } catch (e) {
      setErr(String(e));
    }
  }

  async function clearStorage() {
    await api.clearStorage();
    refreshStats();
  }

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
        One node per network, shared by the wallet (fee estimation + broadcasting) and the
        on-device prover (proof preflight). Leave a field empty to keep that network sign-only.
        The endpoint stays on this device and is never exposed over the wallet's service.
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

      <h3>On-device proving</h3>
      <p className="muted small">
        Local proof generation for SNIP-36 invokes. The prover holds no keys — it proves an
        already-signed transaction. These settings carry a remote-prover API key, so they stay on
        this device and are never exposed over the wallet's service.
      </p>

      <label className="field">
        <span className="muted small">Prover backend (applies after restart)</span>
        <select
          className="input"
          value={prover.prover_backend || "native"}
          onChange={(e) => {
            setProver({ ...prover, prover_backend: e.target.value });
            setProverSaved(false);
          }}
        >
          <option value="native">native — bundled on-device prover (recommended)</option>
          <option value="remote">remote — forward to a configured remote prover</option>
        </select>
      </label>

      {prover.prover_backend === "remote" && (
        <>
          <p className="muted small">
            Remote prover endpoints (used only by the <code>remote</code> backend). The RPC node is
            shared with the wallet above.
          </p>
          {(["testnet", "mainnet"] as const).map((net) => (
            <div key={net} className="field">
              <span className="muted small">
                {net === "testnet" ? "Sepolia (testnet)" : "Mainnet"} — remote prover
              </span>
              <input
                className="input"
                placeholder="Remote prover URL"
                value={prover[net].prover_url}
                onChange={(e) => proverField(net, "prover_url", e.target.value)}
              />
              <input
                className="input"
                type="password"
                placeholder="Remote prover API key"
                autoComplete="off"
                value={prover[net].prover_api_key}
                onChange={(e) => proverField(net, "prover_api_key", e.target.value)}
              />
            </div>
          ))}
        </>
      )}

      <button className="primary" onClick={saveProver}>
        {proverSaved ? "Saved ✓" : "Save prover settings"}
      </button>

      <h3>Proof storage</h3>
      <div className="row">
        <span className="k">Records</span>
        <span className="v">{stats?.records ?? "…"}</span>
      </div>
      <div className="row">
        <span className="k">Size</span>
        <span className="v">{stats ? fmtBytes(stats.bytes) : "…"}</span>
      </div>
      <button className="primary danger" onClick={clearStorage}>
        Clear proof storage
      </button>
    </div>
  );
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}
