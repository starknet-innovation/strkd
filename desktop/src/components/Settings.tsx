import { useEffect, useState } from "react";
import {
  api,
  type Settings as SettingsT,
  type ProverSettings,
  type ProverNetworkConfig,
  type StorageStats,
  type Strk20Config,
  type Strk20PoolInfo,
} from "../api";
import { RevealSeed } from "./RevealSeed";

const EMPTY_STRK20: Strk20Config = {
  pool: "",
  starkscan_api_key: "",
  deposit_prover_url: "",
  avnu_api_key: "",
  paymaster_url: "",
  screener_key: "",
};

/** strkd's self-deployed Sepolia test pool (screener: the public test key 0xCAFEBABE). */
const SEPOLIA_TEST_POOL = "0x03016a46eec8b164e8a89337c048b5cd1463ea4c4de120b9cb76b3df88646323";

const EMPTY_PROVER: ProverSettings = {
  mainnet: { rpc_url: "", prover_url: "", prover_api_key: "", strk20: EMPTY_STRK20 },
  testnet: { rpc_url: "", prover_url: "", prover_api_key: "", strk20: EMPTY_STRK20 },
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
  const [poolInfo, setPoolInfo] = useState<Record<string, Strk20PoolInfo | string>>({});

  useEffect(() => {
    api.getSettings().then(setSettings).catch(() => {});
    api.getProverSettings().then(setProver).catch(() => {});
    refreshStats();
  }, []);

  function refreshStats() {
    api.storageStats().then(setStats).catch(() => {});
  }

  function proverField(
    net: "mainnet" | "testnet",
    key: Exclude<keyof ProverNetworkConfig, "strk20">,
    value: string,
  ) {
    setProver((s) => ({ ...s, [net]: { ...s[net], [key]: value } }));
    setProverSaved(false);
  }

  function strk20Field(net: "mainnet" | "testnet", key: keyof Strk20Config, value: string) {
    setProver((s) => ({
      ...s,
      [net]: { ...s[net], strk20: { ...EMPTY_STRK20, ...s[net].strk20, [key]: value } },
    }));
    setProverSaved(false);
  }

  async function checkPool(net: "mainnet" | "testnet") {
    const c = { ...EMPTY_STRK20, ...prover[net].strk20 };
    setPoolInfo((p) => ({ ...p, [net]: "Checking…" }));
    try {
      const info = await api.strk20CheckPool(
        net,
        c.pool,
        net === "testnet" ? c.screener_key : undefined,
      );
      setPoolInfo((p) => ({ ...p, [net]: info }));
    } catch (e) {
      setPoolInfo((p) => ({ ...p, [net]: String(e) }));
    }
  }

  async function saveProver() {
    setErr("");
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

      <RevealSeed />

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

      <h3>Private balances (STRK20)</h3>
      <p className="muted small">
        Transfers and withdrawals are proved on this device. Deposits must be screened, so they are
        proved remotely: by your deposit prover if set, else by Starkscan with your API key, else by
        the bramble gateway (mainnet only for the last two). That prover sees the deposit, viewing
        key included. Private sends and withdrawals are relayed through AVNU when you set a key;
        without one, prepare them here and submit from another account. Keys stay on this device.
      </p>
      {(["testnet", "mainnet"] as const).map((net) => {
        const c = { ...EMPTY_STRK20, ...prover[net].strk20 };
        return (
          <div key={net} className="field">
            <span className="muted small">{net === "testnet" ? "Sepolia (testnet)" : "Mainnet"}</span>
            <input
              className="input"
              placeholder="Pool address (empty = canonical pool)"
              value={c.pool}
              onChange={(e) => strk20Field(net, "pool", e.target.value)}
            />
            {c.pool.trim() !== "" && (
              <p className="error small">
                Not the canonical pool: every STRK20 call on this network, from this window and from
                paired agents, uses this contract. Approval prompts flag it.
              </p>
            )}
            <button className="small-btn" onClick={() => checkPool(net)}>
              Check pool
            </button>
            <PoolInfoView info={poolInfo[net]} />
            <input
              className="input"
              type="password"
              autoComplete="off"
              placeholder="Starkscan API key (prove scope)"
              value={c.starkscan_api_key}
              onChange={(e) => strk20Field(net, "starkscan_api_key", e.target.value)}
            />
            <input
              className="input"
              placeholder="Deposit prover URL (optional, overrides Starkscan/bramble)"
              value={c.deposit_prover_url}
              onChange={(e) => strk20Field(net, "deposit_prover_url", e.target.value)}
            />
            <input
              className="input"
              type="password"
              autoComplete="off"
              placeholder="AVNU paymaster API key"
              value={c.avnu_api_key}
              onChange={(e) => strk20Field(net, "avnu_api_key", e.target.value)}
            />
            <input
              className="input"
              placeholder="Paymaster URL (empty = AVNU)"
              value={c.paymaster_url}
              onChange={(e) => strk20Field(net, "paymaster_url", e.target.value)}
            />
            {net === "testnet" && (
              <>
                <input
                  className="input"
                  type="password"
                  autoComplete="off"
                  placeholder="Test pool screener key (testnet only; optional)"
                  value={c.screener_key}
                  onChange={(e) => strk20Field(net, "screener_key", e.target.value)}
                />
                <p className="muted small">
                  For a pool you deployed yourself with your own screener key (strkd's test pool{" "}
                  <code>{SEPOLIA_TEST_POOL.slice(0, 10)}…</code> uses the public test key{" "}
                  <code>0xCAFEBABE</code>). Deposits are then proved on this device and strkd signs the
                  screening attestation, so nothing leaves the machine.{" "}
                  <strong>
                    Never enter a real screener's key: it is stored unencrypted in this device's
                    settings file (owner-only), like the API keys.
                  </strong>{" "}
                  Refused on mainnet; checked against the pool before each deposit.
                </p>
              </>
            )}
          </div>
        );
      })}
      <button className="primary" onClick={saveProver}>
        {proverSaved ? "Saved ✓" : "Save STRK20 settings"}
      </button>
      {err && <p className="error">{err}</p>}

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

function PoolInfoView({ info }: { info?: Strk20PoolInfo | string }) {
  if (!info) return null;
  if (typeof info === "string") return <p className="muted small">{info}</p>;
  return (
    <div className="small">
      {info.deployed && (
        <p className="muted small">
          {info.known_class ? "Privacy pool (known class)" : "Unknown class"}
          {info.version ? `, version ${info.version}` : ""}
          {info.fee_amount != null ? `, fee ${info.fee_amount} (base units of STRK)` : ""}
          {info.screener_matches === true ? ". The screener key matches this pool." : "."}
        </p>
      )}
      {info.warnings.map((w) => (
        <p key={w} className="error small">
          {w}
        </p>
      ))}
    </div>
  );
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}
