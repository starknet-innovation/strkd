import { useEffect, useRef, useState } from "react";
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

/// Format a fri (1e-18 STRK) string as "1.2345 STRK". Uses BigInt so large
/// balances don't lose precision. Returns "" for unknown (no node).
function formatStrk(fri: string | null | undefined): string {
  if (fri == null) return "";
  try {
    const v = BigInt(fri);
    const whole = v / 10n ** 18n;
    const frac = ((v % 10n ** 18n) * 10000n) / 10n ** 18n;
    return `${whole}.${frac.toString().padStart(4, "0")} STRK`;
  } catch {
    return "";
  }
}

// How often node-derived data (deployment status + balance) is re-fetched while
// the tab is open. Each fetch hits the node fresh at the `latest` block.
const REFRESH_MS = 10000;

export function Accounts({ status, onChange }: { status: Status | null; onChange: () => void }) {
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [label, setLabel] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  // address → deployment status: true (deployed), false (undeployed), null (no node / unknown)
  const [deployed, setDeployed] = useState<Record<string, boolean | null>>({});
  // address → STRK balance in fri (string), or null when unknown (no node).
  const [balances, setBalances] = useState<Record<string, string | null>>({});
  const [deploying, setDeploying] = useState<string | null>(null);
  // Broadcast accepted, awaiting on-chain confirmation (so we don't revert to a
  // "Deploy" button and let the user submit a second, nonce-conflicting tx).
  const [pending, setPending] = useState<Record<string, boolean>>({});
  // address whose Deploy button is wiggling (clicked while unfunded).
  const [wiggle, setWiggle] = useState<string | null>(null);

  const accountsRef = useRef<Account[]>([]);
  accountsRef.current = accounts;

  // The active network drives deploy-status + where operations execute.
  const activeNet: "mainnet" | "testnet" = status?.network === "SN_MAIN" ? "mainnet" : "testnet";

  // Fetch node-derived data (deploy status + balance) for each account, updating
  // in place. When an account turns deployed, clear any "pending" marker.
  const fetchStatuses = (accts: Account[]) => {
    accts.forEach((a) => {
      api
        .deployStatus(a.address)
        .then((s) => {
          setDeployed((cur) => ({ ...cur, [a.address]: s }));
          if (s === true) {
            setPending((cur) => {
              if (!cur[a.address]) return cur;
              const next = { ...cur };
              delete next[a.address];
              return next;
            });
          }
        })
        .catch(() => {});
      api
        .balance(a.address)
        .then((b) => setBalances((cur) => ({ ...cur, [a.address]: b })))
        .catch(() => {});
    });
  };

  const load = () =>
    api
      .listAccounts()
      .then((a) => {
        setAccounts(a);
        fetchStatuses(a);
      })
      .catch(() => {});

  // Reload whenever the active network changes (deploy-status + balance differ
  // per network). Clear the per-network maps so stale values don't flash.
  useEffect(() => {
    setDeployed({});
    setBalances({});
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeNet]);

  // Periodic refresh so the UI reflects funding/deployment without a manual
  // reload (answers "how often does it refresh?": every REFRESH_MS, on `latest`).
  useEffect(() => {
    const id = setInterval(() => {
      if (accountsRef.current.length) fetchStatuses(accountsRef.current);
    }, REFRESH_MS);
    return () => clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
    // Guard: refuse to deploy an unfunded account (it pays its own deploy fee).
    // Wiggle the button + explain instead of letting it fail on-chain.
    if (balances[address] === "0") {
      setWiggle(address);
      setErr("This account has no STRK — fund it first (it pays its own deploy fee).");
      setTimeout(() => setWiggle((w) => (w === address ? null : w)), 600);
      return;
    }
    setDeploying(address);
    setErr("");
    try {
      await api.deployAccount(address);
      // Broadcast accepted → mark pending; the periodic refresh flips it to
      // "deployed" once the node sees it. We do NOT revert to a Deploy button.
      setPending((cur) => ({ ...cur, [address]: true }));
      fetchStatuses([{ address } as Account]);
    } catch (e) {
      setErr(String(e));
      // Re-check: a confusing error may mean it actually deployed already.
      api
        .deployStatus(address)
        .then((s) => setDeployed((cur) => ({ ...cur, [address]: s })))
        .catch(() => {});
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
        balance + deployment status for {activeNet}.
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
        {accounts.map((a) => {
          const isPending = !!pending[a.address] || deploying === a.address;
          const bal = balances[a.address];
          return (
            <li key={a.address} className="acct">
              <div className="acct-main">
                <span className="acct-label">{a.label}</span>
                <span className={`badge ${a.domain}`}>{a.domain}</span>
                {deployed[a.address] === false && !isPending && (
                  <span className="badge err">undeployed</span>
                )}
                {deployed[a.address] === true && <span className="badge ok">deployed</span>}
                <span className="spacer" />
                {bal != null && <span className="bal muted small">{formatStrk(bal)}</span>}
                {(deployed[a.address] === false || isPending) && (
                  <button
                    className={`primary small-btn ${wiggle === a.address ? "wiggle" : ""}`}
                    disabled={isPending}
                    title={bal === "0" ? "Fund this account first" : "Deploy this account"}
                    onClick={() => deploy(a.address)}
                  >
                    {isPending ? "Deploying…" : "Deploy"}
                  </button>
                )}
              </div>
              <div className="addr-row">
                <code className="addr">{a.address}</code>
                <CopyAddr address={a.address} />
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
