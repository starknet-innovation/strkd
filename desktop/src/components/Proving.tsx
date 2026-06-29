import { useEffect, useState } from "react";
import { api, type Activity, type ProofRecord, type ProofSummary, type ProverStatus } from "../api";

/// On-device proving panel: backend status + a live/historical proof feed.
/// Proving holds no key material — it proves an already-signed payload. Agents
/// reach it via `companion_prove` on the loopback service; this panel is the
/// human-facing view (status, a test prove, and what's been proven).
export function Proving() {
  const [status, setStatus] = useState<ProverStatus | null>(null);
  const [proofs, setProofs] = useState<ProofSummary[]>([]);
  const [live, setLive] = useState<Activity[]>([]);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [detail, setDetail] = useState<ProofRecord | null>(null);
  const [now, setNow] = useState(Date.now());

  useEffect(() => {
    let alive = true;
    const tick = () => {
      api.proverStatus().then((s) => alive && setStatus(s)).catch(() => {});
      api.listProofs().then((p) => alive && setProofs(p)).catch(() => {});
      api.proofActivity().then((a) => alive && setLive(a)).catch(() => {});
    };
    tick();
    const poll = setInterval(tick, 2500);
    const clock = setInterval(() => alive && setNow(Date.now()), 1000); // elapsed timer
    return () => {
      alive = false;
      clearInterval(poll);
      clearInterval(clock);
    };
  }, []);

  // In-progress jobs: latest status per job (feed is newest-first), kept only if
  // still queued/proving and not yet persisted as a completed proof.
  const done = new Set(proofs.map((p) => p.job_id));
  const seen = new Set<string>();
  const inProgress = live.filter((a) => {
    if (seen.has(a.job_id)) return false;
    seen.add(a.job_id);
    return (a.status === "queued" || a.status === "proving") && !done.has(a.job_id);
  });

  const toggle = (id: string) => {
    if (expanded === id) {
      setExpanded(null);
      setDetail(null);
      return;
    }
    setExpanded(id);
    setDetail(null);
    api.proofDetail(id).then(setDetail).catch(() => {});
  };

  const badgeClass = (s: ProofSummary["status"]) =>
    s === "succeeded" ? "badge ok" : s === "failed" ? "badge err" : "badge proving";

  return (
    <div className="panel">
      <div className="row">
        <span className="k">Prover backend</span>
        <span className="v">{status?.prover ?? "…"}</span>
      </div>
      <div className="row">
        <span className="k">Ready</span>
        <span className="v">{status ? (status.ready ? "yes" : "no") : "…"}</span>
      </div>

      <button className="primary" onClick={() => void api.testProve()}>
        Run a test proof
      </button>

      <p className="muted small">
        Paired agents prove over the wallet's loopback service: <code>companion_signAndProve</code>{" "}
        signs a SNIP-36 virtual tx and proves it in one call, or <code>companion_prove</code> proves
        an already-signed payload. The prover holds no keys — it only proves. Configure the backend +
        per-network RPC in <strong>Settings</strong>.
      </p>

      <h3>Proofs</h3>
      {proofs.length === 0 && inProgress.length === 0 ? (
        <p className="muted small">No proofs yet. Try “Run a test proof”.</p>
      ) : (
        <ul className="feed">
          {inProgress.map((a) => (
            <li key={`live-${a.job_id}`} className="feed-row">
              <span className="badge proving">{a.status}</span>
              <span className="mono">{a.job_id}</span>
              {a.label && <span className="muted small">{a.label}</span>}
              <span className="spacer" />
              <span className="muted small mono">{fmtElapsed(now - a.started_at_ms)}</span>
            </li>
          ))}

          {proofs.map((p) => (
            <li key={p.job_id} className="proof">
              <div className="feed-row clickable" onClick={() => toggle(p.job_id)}>
                <span className={badgeClass(p.status)}>{p.status}</span>
                <span className="chip">{p.network}</span>
                {p.label && <span className="muted small">{p.label}</span>}
                <span className="spacer" />
                <span className="muted small mono">{fmtDur(p.prove_ms)}</span>
                <span className="muted small">{fmtAgo(p.created_at_ms)}</span>
              </div>

              {expanded === p.job_id && (
                <div className="detail">
                  <Row k="Job" v={p.job_id} />
                  <Row k="When" v={new Date(p.created_at_ms).toLocaleString()} />
                  <Row k="Network" v={p.network} />
                  <Row k="Proof time" v={fmtDur(p.prove_ms)} />
                  <Row k="Proof size" v={p.proof_bytes ? fmtBytes(p.proof_bytes) : "—"} />
                  {p.error && <Row k="Error" v={p.error} />}
                  {detail?.proof && (
                    <>
                      <Row k="Proof facts" v={`${detail.proof.proof_facts?.length ?? 0}`} />
                      <Row k="L2→L1 messages" v={`${detail.proof.l2_to_l1_messages?.length ?? 0}`} />
                    </>
                  )}
                  {!detail && <p className="muted small">loading detail…</p>}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function Row({ k, v }: { k: string; v: string }) {
  return (
    <div className="row">
      <span className="k">{k}</span>
      <span className="v mono">{v}</span>
    </div>
  );
}

function fmtDur(ms: number): string {
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`;
}
function fmtElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
}
function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}
function fmtAgo(ms: number): string {
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}
