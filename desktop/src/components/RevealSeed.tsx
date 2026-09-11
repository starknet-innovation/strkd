import { useEffect, useRef, useState } from "react";
import { api } from "../api";

// Recovery-phrase reveal (#28). Deliberately IPC-only — the loopback service
// has no equivalent and must never get one.
//
// Three things this screen is careful about:
//  - it starts collapsed, so the phrase is never a stray click away;
//  - it re-authenticates every time, because "the app is unlocked" is not the
//    same as "the person at the keyboard asked for this";
//  - it clears itself on hide, on unmount, and after a timeout, so it cannot be
//    left on screen.

/** Auto-hide after this long, so an unattended window doesn't keep it up. */
const AUTO_HIDE_MS = 120_000;

export function RevealSeed() {
  const [open, setOpen] = useState(false);
  const [passphrase, setPassphrase] = useState("");
  const [phrase, setPhrase] = useState<string | null>(null);
  const [blurred, setBlurred] = useState(true);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const timer = useRef<number | null>(null);

  const clear = () => {
    setPhrase(null);
    setPassphrase("");
    setBlurred(true);
    setErr("");
    if (timer.current !== null) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
  };

  // Never survive leaving the tab.
  useEffect(() => () => clear(), []);

  const reveal = async () => {
    setErr("");
    setBusy(true);
    try {
      const p = await api.revealSeed(passphrase);
      setPhrase(p);
      setBlurred(true);
      // The passphrase has done its job; don't keep it in component state.
      setPassphrase("");
      timer.current = window.setTimeout(clear, AUTO_HIDE_MS);
    } catch (e) {
      setErr(String(e));
      setPhrase(null);
    } finally {
      setBusy(false);
    }
  };

  if (!open) {
    return (
      <div style={{ marginTop: 12 }}>
        <button onClick={() => setOpen(true)}>Show recovery phrase…</button>
        <p className="muted small" style={{ marginTop: 4 }}>
          Needed to move this wallet to another device or to Bramble.
        </p>
      </div>
    );
  }

  return (
    <div
      style={{
        marginTop: 12,
        border: "1px solid var(--err)",
        borderRadius: 8,
        padding: 12,
      }}
    >
      <strong>Anyone who reads this phrase controls every account in this wallet.</strong>
      <p className="muted small">
        It is not a password and cannot be changed — it <em>is</em> the wallet. Write it down
        somewhere offline. Don't photograph it, don't put it in a note-taking app, and don't paste
        it into a chat, including one with an AI assistant.
      </p>

      {phrase === null ? (
        <>
          <label className="field">
            <span className="muted small">Confirm your vault passphrase</span>
            <input
              className="input"
              type="password"
              value={passphrase}
              autoFocus
              onChange={(e) => setPassphrase(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && passphrase && !busy) void reveal();
              }}
            />
          </label>
          <div style={{ marginTop: 8 }}>
            <button
              className="primary"
              style={{ background: "var(--err)", color: "#fff" }}
              onClick={reveal}
              disabled={!passphrase || busy}
            >
              {busy ? "Checking…" : "Reveal"}
            </button>
            <button
              className="ghost"
              style={{ marginLeft: 8 }}
              onClick={() => {
                clear();
                setOpen(false);
              }}
            >
              Cancel
            </button>
          </div>
        </>
      ) : (
        <>
          <div
            onClick={() => setBlurred(false)}
            role="button"
            tabIndex={0}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") setBlurred(false);
            }}
            title={blurred ? "Click to reveal" : undefined}
            style={{
              marginTop: 8,
              padding: 12,
              background: "var(--bg)",
              border: "1px solid var(--line)",
              borderRadius: 8,
              fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
              fontSize: 14,
              lineHeight: 1.8,
              wordSpacing: "0.4em",
              cursor: blurred ? "pointer" : "text",
              userSelect: blurred ? "none" : "text",
              filter: blurred ? "blur(7px)" : "none",
              transition: "filter 200ms cubic-bezier(0.2, 0.7, 0.3, 1)",
            }}
          >
            {phrase}
          </div>
          <p className="muted small" style={{ marginTop: 4 }}>
            {blurred
              ? "Click to reveal. Check nobody is looking over your shoulder or sharing your screen."
              : "Hides automatically after two minutes."}
          </p>
          <p className="muted small">
            There's no copy button on purpose: the clipboard is readable by every other program on
            this machine, including the agents this wallet talks to. Write it out by hand.
          </p>
          <button
            style={{ marginTop: 8 }}
            onClick={() => {
              clear();
              setOpen(false);
            }}
          >
            Hide
          </button>
        </>
      )}

      {err && <p className="error">{err}</p>}
    </div>
  );
}
