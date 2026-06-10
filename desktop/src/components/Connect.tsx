import { useState } from "react";
import { type Status } from "../api";

/// Shows the local endpoint and a ready-to-paste prompt that teaches an agent
/// how to use the wallet (it points the agent at `GET /` for full usage).
export function Connect({ status }: { status: Status | null }) {
  const [copied, setCopied] = useState(false);
  if (!status) return <p className="muted">Connecting…</p>;

  const url = status.service_url;
  const prompt =
    `You have a local Starknet wallet companion "strkd" at ${url}.\n` +
    `GET ${url}/ for usage and follow it. It manages Starknet accounts and signs ` +
    `transactions on your behalf; every sensitive action asks the human for on-screen ` +
    `approval, and private keys never leave the wallet.\n` +
    `Pair once (companion_requestPairing), then you can create an account, request STRK ` +
    `funding (companion_requestFunding), and request signatures.`;

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(prompt);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard unavailable */
    }
  };

  return (
    <div className="panel">
      <div className="row">
        <span className="k">Local endpoint</span>
        <code className="v mono">{url}</code>
      </div>
      <div className="row">
        <span className="k">Node (broadcast)</span>
        <span className="v">
          {status.node_configured ? "configured" : "not set — sign-only"}
        </span>
      </div>
      <p className="muted small">
        Agents and apps on this machine call the wallet here. Point any client at{" "}
        <code>{url}/</code> — it returns its own usage.
        {!status.node_configured &&
          " Set STRKD_RPC_URL (a Starknet RPC endpoint) to enable fee estimation and broadcasting."}
      </p>

      <div className="prompt-block">
        <p className="muted small">Copy-paste this prompt for your agent to use strkd:</p>
        <pre className="prompt">{prompt}</pre>
        <button className="primary" onClick={() => void copy()}>
          {copied ? "Copied ✓" : "Copy prompt"}
        </button>
      </div>
    </div>
  );
}
