import { useEffect, useRef, useState } from "react";
import {
  api,
  onSweepProgress,
  type AccountPlan,
  type Status,
  type SweepEvent,
  type SweepPlan,
  type SweepReport,
} from "../api";

// TEMPORARY PANEL — issue #14. This exists only to recover assets before the
// account-derivation change in issue #16 moves every address. Delete this file,
// its tab in App.tsx, and the sweep module it calls once the cutover is done.

/** Raw token amount → human string, via BigInt so large balances keep precision. */
function fmt(raw: string, decimals: number): string {
  try {
    const v = BigInt(raw);
    const scale = 10n ** BigInt(decimals);
    const whole = v / scale;
    const frac = ((v % scale) * 10000n) / scale;
    return `${whole}.${frac.toString().padStart(4, "0")}`;
  } catch {
    return raw;
  }
}

function short(addr: string): string {
  return addr.length > 16 ? `${addr.slice(0, 10)}…${addr.slice(-6)}` : addr;
}

function Row({ p }: { p: AccountPlan }) {
  const blocked = p.blockers.length > 0;
  return (
    <tr style={{ opacity: blocked || p.balances.length === 0 ? 0.55 : 1 }}>
      <td>
        <div>{p.label}</div>
        <div className="muted" style={{ fontSize: 11 }}>
          {short(p.address)} · {p.domain}/{p.index}
          {p.is_funding_source && " · funds the others"}
        </div>
      </td>
      <td>
        {p.balances.length === 0 ? (
          <span className="muted">empty</span>
        ) : (
          p.balances.map((b) => (
            <div key={b.token}>
              {fmt(b.amount, 18)} {b.symbol}
            </div>
          ))
        )}
      </td>
      <td>
        {!p.deployed && <div>not deployed</div>}
        {p.needs_deploy && <div className="muted">will be deployed</div>}
        {p.needs_gas !== "0" && <div className="muted">needs {fmt(p.needs_gas, 18)} STRK gas</div>}
        {blocked && (
          <div style={{ color: "var(--bramble-danger)", fontSize: "var(--type-micro)" }}>{p.blockers.join("; ")}</div>
        )}
      </td>
    </tr>
  );
}

export function Sweep({ status }: { status: Status | null }) {
  const [destination, setDestination] = useState("");
  const [plan, setPlan] = useState<SweepPlan | null>(null);
  const [report, setReport] = useState<SweepReport | null>(null);
  const [busy, setBusy] = useState<"" | "planning" | "running">("");
  const [err, setErr] = useState("");
  const [confirmText, setConfirmText] = useState("");
  const [progress, setProgress] = useState<string[]>([]);
  const unlisten = useRef<(() => void) | null>(null);

  const isMainnet = status?.network === "SN_MAIN";

  useEffect(() => {
    onSweepProgress((e: SweepEvent) => {
      setProgress((prev) => [...prev, describe(e)]);
    }).then((un) => {
      unlisten.current = un;
    });
    return () => unlisten.current?.();
  }, []);

  // A fresh destination invalidates any plan already on screen, so the confirm
  // step can never apply to an address the user has since changed.
  useEffect(() => {
    setPlan(null);
    setReport(null);
    setConfirmText("");
  }, [destination]);

  const doPlan = async () => {
    setErr("");
    setReport(null);
    setBusy("planning");
    try {
      setPlan(await api.sweepPlan(destination.trim()));
    } catch (e) {
      setErr(String(e));
      setPlan(null);
    } finally {
      setBusy("");
    }
  };

  const doExecute = async () => {
    setErr("");
    setProgress([]);
    setBusy("running");
    try {
      setReport(await api.sweepExecute(destination.trim()));
      setPlan(null);
      setConfirmText("");
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy("");
    }
  };

  const sweepable = plan?.accounts.filter((a) => a.balances.length > 0 && !a.is_destination) ?? [];
  const confirmed = confirmText.trim().toUpperCase() === "SWEEP";

  return (
    <div className="panel">
      <h2>Sweep (temporary)</h2>
      <p className="muted">
        Moves every ERC-20 balance this wallet can reach into one address, deploying and
        topping up accounts as needed. It exists so assets survive the coming account-derivation
        change, and it will be removed afterwards.
      </p>

      <div
        style={{
          border: "1px solid var(--bramble-danger)",
          borderRadius: "var(--radius-sm)",
          padding: "var(--space-sm) var(--space-md)",
          margin: "var(--space-md) 0",
          fontSize: "var(--type-caption)",
        }}
      >
        <strong>This cannot be undone.</strong> Funds are sent to the address below and deploys
        are permanent. Check the destination character by character — a typo sends everything to
        an address nobody controls.
        {isMainnet && (
          <div style={{ marginTop: "var(--space-xs)", color: "var(--bramble-danger)" }}>
            <strong>You are on mainnet.</strong> This moves real funds.
          </div>
        )}
      </div>

      <label className="muted" style={{ fontSize: 12 }}>
        Destination address
      </label>
      <input
        className="input"
        style={{ fontFamily: "var(--font-mono)" }}
        placeholder="0x…"
        value={destination}
        onChange={(e) => setDestination(e.target.value)}
        spellCheck={false}
      />

      <div style={{ marginTop: 8 }}>
        <button onClick={doPlan} disabled={!destination.trim() || busy !== ""}>
          {busy === "planning" ? "Checking…" : "Check what would move"}
        </button>
      </div>

      {err && <p style={{ color: "var(--bramble-danger)" }}>{err}</p>}

      {plan && (
        <>
          <h3 style={{ marginTop: 16 }}>
            {sweepable.length} account(s) on {plan.network}
          </h3>

          {plan.warnings.map((w) => (
            <p key={w} style={{ color: "var(--bramble-danger)", fontSize: "var(--type-caption)" }}>
              ⚠ {w}
            </p>
          ))}

          <table className="log">
            <thead>
              <tr>
                <th>Account</th>
                <th>Holds</th>
                <th>Notes</th>
              </tr>
            </thead>
            <tbody>
              {plan.accounts.map((p) => (
                <Row key={p.address} p={p} />
              ))}
            </tbody>
          </table>

          <p style={{ marginTop: 10 }}>
            <strong>Total to move:</strong>{" "}
            {plan.totals.length === 0
              ? "nothing"
              : plan.totals.map((t) => `${fmt(t.amount, 18)} ${t.symbol}`).join(" · ")}
          </p>
          <p className="muted" style={{ fontSize: 12 }}>
            Destination <code>{plan.destination}</code>
            <br />
            Funding source holds {fmt(plan.funding_available, 18)} STRK, needs about{" "}
            {fmt(plan.funding_required, 18)} STRK for deploys and top-ups.
          </p>

          {plan.totals.length > 0 && (
            <div style={{ marginTop: 12 }}>
              <label className="muted" style={{ fontSize: 12 }}>
                Type SWEEP to confirm
              </label>
              <input
                className="input"
                style={{ width: 160 }}
                value={confirmText}
                onChange={(e) => setConfirmText(e.target.value)}
                placeholder="SWEEP"
                spellCheck={false}
              />
              <button
                className="primary danger"
                style={{ marginLeft: 8 }}
                onClick={doExecute}
                disabled={!confirmed || busy !== ""}
              >
                {busy === "running" ? "Sweeping…" : `Sweep to ${short(plan.destination)}`}
              </button>
            </div>
          )}
        </>
      )}

      {progress.length > 0 && (
        <>
          <h3 style={{ marginTop: 16 }}>Progress</h3>
          <pre style={{ maxHeight: 200, overflow: "auto", fontSize: 11 }}>
            {progress.join("\n")}
          </pre>
        </>
      )}

      {report && (
        <>
          <h3 style={{ marginTop: 16 }}>
            Done — {report.swept} swept, {report.skipped} skipped, {report.failed} failed
          </h3>
          <table className="log">
            <thead>
              <tr>
                <th>Account</th>
                <th>Result</th>
                <th>Transactions</th>
              </tr>
            </thead>
            <tbody>
              {report.outcomes.map((o) => (
                <tr key={o.address}>
                  <td>
                    {o.label}
                    <div className="muted" style={{ fontSize: 11 }}>
                      {short(o.address)}
                    </div>
                  </td>
                  <td>
                    <div>{o.status}</div>
                    {o.moved.map((m) => (
                      <div key={m.token} className="muted" style={{ fontSize: 11 }}>
                        {fmt(m.amount, 18)} {m.symbol}
                      </div>
                    ))}
                    {o.detail && (
                      <div className="muted" style={{ fontSize: 11 }}>
                        {o.detail}
                      </div>
                    )}
                  </td>
                  <td style={{ fontSize: "var(--type-micro)", fontFamily: "var(--font-mono)" }}>
                    {o.transactions.map((t) => (
                      <div key={t}>{short(t)}</div>
                    ))}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}
    </div>
  );
}

function describe(e: SweepEvent): string {
  switch (e.kind) {
    case "started":
      return `sweeping ${e.accounts} account(s)`;
    case "step":
      return `${short(e.address)} — ${e.step}`;
    case "sent":
      return `${short(e.address)} — ${e.what} sent (${short(e.tx)})`;
    case "waiting":
      return `waiting for ${short(e.tx)}`;
    case "skipped":
      return `${short(e.address)} — skipped: ${e.why}`;
    case "failed":
      return `${short(e.address)} — FAILED: ${e.why}`;
    case "done":
      return `finished: ${e.swept} swept, ${e.skipped} skipped, ${e.failed} failed`;
  }
}
