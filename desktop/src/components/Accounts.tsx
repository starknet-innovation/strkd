import { useEffect, useState } from "react";
import { api, type Account, type Status } from "../api";

function CopyAddr({ address }: { address: string }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(address);
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    } catch {
      /* clipboard unavailable */
    }
  };
  return (
    <button className="copybtn" title="Copy address" aria-label="Copy address" onClick={copy}>
      {copied ? "✓" : "⧉"}
    </button>
  );
}

export function Accounts({ status, onChange }: { status: Status | null; onChange: () => void }) {
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [label, setLabel] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  // address → deployment status: true (deployed), false (undeployed), null (no node / unknown)
  const [deployed, setDeployed] = useState<Record<string, boolean | null>>({});
  const [deploying, setDeploying] = useState<string | null>(null);

  // The active network drives deploy-status + where operations execute.
  const activeNet: "mainnet" | "testnet" = status?.network === "SN_MAIN" ? "mainnet" : "testnet";

  const loadStatuses = (accts: Account[]) => {
    setDeployed({}); // clear — deployment status is per-network
    accts.forEach((a) => {
      api
        .deployStatus(a.address)
        .then((s) => setDeployed((cur) => ({ ...cur, [a.address]: s })))
        .catch(() => {});
    });
  };

  const load = () =>
    api
      .listAccounts()
      .then((a) => {
        setAccounts(a);
        loadStatuses(a);
      })
      .catch(() => {});

  // Reload whenever the active network changes (deploy-status differs per net).
  useEffect(() => {
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeNet]);

  async function selectNetwork(net: "mainnet" | "testnet") {
    if (net === activeNet) return;
    setErr("");
    try {
      await api.setNetwork(net);
      onChange(); // refresh status → activeNet flips → useEffect reloads
    } catch (e) {
      setErr(String(e));
    }
  }

  async function deploy(address: string) {
    setDeploying(address);
    setErr("");
    try {
      await api.deployAccount(address);
      // Re-check status after broadcasting.
      const s = await api.deployStatus(address);
      setDeployed((cur) => ({ ...cur, [address]: s }));
    } catch (e) {
      setErr(String(e));
    } finally {
      setDeploying(null);
    }
  }

  async function add() {
    setBusy(true);
    setErr("");
    try {
      await api.addUserAccount(label || `Account ${accounts.length}`);
      setLabel("");
      await load();
      onChange();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="panel">
      <div className="subtabs">
        <button
          className={activeNet === "testnet" ? "subtab active" : "subtab"}
          onClick={() => selectNetwork("testnet")}
        >
          Testnet
        </button>
        <button
          className={activeNet === "mainnet" ? "subtab active" : "subtab"}
          onClick={() => selectNetwork("mainnet")}
        >
          Mainnet
        </button>
      </div>
      <p className="muted small">
        Same accounts on both networks; this selects where deploy/fund/sign run and shows
        deployment status for {activeNet}.
      </p>
      <div className="addrow">
        <input
          className="input"
          placeholder="Label (optional)"
          value={label}
          onChange={(e) => setLabel(e.target.value)}
        />
        <button className="primary" disabled={busy} onClick={add}>
          Add account
        </button>
      </div>
      {err && <p className="error">{err}</p>}
      <ul className="list">
        {accounts.length === 0 && <li className="muted">No accounts yet.</li>}
        {accounts.map((a) => (
          <li key={a.address} className="acct">
            <div className="acct-main">
              <span className="acct-label">{a.label}</span>
              <span className={`badge ${a.domain}`}>{a.domain}</span>
              {deployed[a.address] === false && <span className="badge err">undeployed</span>}
              <span className="spacer" />
              {deployed[a.address] === false && (
                <button
                  className="primary small-btn"
                  disabled={deploying === a.address}
                  onClick={() => deploy(a.address)}
                >
                  {deploying === a.address ? "Deploying…" : "Deploy"}
                </button>
              )}
            </div>
            <div className="addr-row">
              <code className="addr">{a.address}</code>
              <CopyAddr address={a.address} />
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}
