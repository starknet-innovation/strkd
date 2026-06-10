import { useState } from "react";
import { api } from "../api";

export function Onboarding({ onDone }: { onDone: () => void }) {
  const [mode, setMode] = useState<"choose" | "generate" | "import">("choose");
  const [mnemonic, setMnemonic] = useState("");
  const [phrase, setPhrase] = useState("");
  const [staged, setStaged] = useState(false);
  const [pass, setPass] = useState("");
  const [pass2, setPass2] = useState("");
  const [err, setErr] = useState("");

  async function doGenerate() {
    setErr("");
    try {
      const m = await api.generate(12);
      setMnemonic(m);
      setMode("generate");
      setStaged(true);
    } catch (e) {
      setErr(String(e));
    }
  }

  async function doImport() {
    setErr("");
    try {
      await api.import(phrase.trim());
      setStaged(true);
    } catch (e) {
      setErr(String(e));
    }
  }

  async function finalize() {
    setErr("");
    if (pass.length < 8) return setErr("passphrase must be at least 8 characters");
    if (pass !== pass2) return setErr("passphrases do not match");
    try {
      await api.finalizeSetup(pass);
      onDone();
    } catch (e) {
      setErr(String(e));
    }
  }

  if (mode === "choose") {
    return (
      <div className="panel">
        <h2>Welcome</h2>
        <p className="muted">Set up your Starknet wallet. The seed is encrypted on this device.</p>
        <button className="primary" onClick={doGenerate}>
          Create a new wallet
        </button>
        <button className="ghost" onClick={() => setMode("import")}>
          Import an existing seed
        </button>
        {err && <p className="error">{err}</p>}
      </div>
    );
  }

  return (
    <div className="panel">
      {mode === "generate" && (
        <>
          <h2>Back up your seed</h2>
          <p className="muted small">
            Write these words down and keep them safe. They will not be shown again.
          </p>
          <pre className="mnemonic">{mnemonic}</pre>
        </>
      )}

      {mode === "import" && !staged && (
        <>
          <h2>Import seed</h2>
          <textarea
            className="input"
            rows={3}
            placeholder="12 or 24 words, space-separated"
            value={phrase}
            onChange={(e) => setPhrase(e.target.value)}
          />
          <button className="primary" onClick={doImport}>
            Continue
          </button>
        </>
      )}

      {staged && (
        <>
          <h3>Set a passphrase</h3>
          <p className="muted small">Encrypts the seed at rest. You'll enter it to unlock.</p>
          <input
            className="input"
            type="password"
            placeholder="Passphrase"
            value={pass}
            onChange={(e) => setPass(e.target.value)}
          />
          <input
            className="input"
            type="password"
            placeholder="Confirm passphrase"
            value={pass2}
            onChange={(e) => setPass2(e.target.value)}
          />
          <button className="primary" onClick={finalize}>
            Create wallet
          </button>
        </>
      )}

      {err && <p className="error">{err}</p>}
    </div>
  );
}
