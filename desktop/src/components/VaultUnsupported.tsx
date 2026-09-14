import { useState } from "react";
import { api } from "../api";

// Shown when a vault exists but this build refuses its version (#16 changed how
// accounts are derived, so old vaults are deliberately not opened).
//
// This screen exists because without it the app is a dead end: onboarding only
// appears when no vault file exists, so a refused vault means the unlock screen
// forever. The old file is moved aside, never deleted — it may be someone's only
// copy of their seed.

export function VaultUnsupported({ onArchived }: { onArchived: () => void }) {
  const [busy, setBusy] = useState(false);
  const [confirmText, setConfirmText] = useState("");
  const [err, setErr] = useState("");
  const confirmed = confirmText.trim().toUpperCase() === "START OVER";

  const archive = async () => {
    setErr("");
    setBusy(true);
    try {
      await api.archiveUnsupportedVault();
      onArchived();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="panel">
      <h2>This wallet needs to be set up again</h2>

      <p>
        Your vault was made by an earlier version of strkd. This version changed how accounts are
        derived from your recovery phrase, so it deliberately won't open older vaults — opening one
        would point the wallet at accounts that are no longer yours.
      </p>

      <p>
        <strong>Nothing is damaged and your passphrase is not wrong.</strong> Your recovery phrase
        still controls everything it always did.
      </p>

      <p className="muted small">
        Your existing vault file is kept as a backup next to the original, with its version and a
        timestamp in the name. It is never deleted, so an older build can still open it.
      </p>

      <p>
        To continue you'll set the wallet up again and <strong>import your recovery phrase</strong>.
        Make sure you have it in front of you first — once the vault is moved aside, the phrase is
        the only way back in.
      </p>

      <label className="field">
        <span className="muted small">
          Type START OVER to confirm you have your recovery phrase
        </span>
        <input
          className="input"
          style={{ maxWidth: 220 }}
          value={confirmText}
          onChange={(e) => setConfirmText(e.target.value)}
          spellCheck={false}
        />
      </label>

      <button className="primary" onClick={archive} disabled={!confirmed || busy}>
        {busy ? "Working…" : "Set up again"}
      </button>

      {err && <p className="error">{err}</p>}
    </div>
  );
}
