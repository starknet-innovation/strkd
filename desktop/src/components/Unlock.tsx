import { useState } from "react";
import { api } from "../api";

export function Unlock({ onUnlocked }: { onUnlocked: () => void }) {
  const [pass, setPass] = useState("");
  const [err, setErr] = useState("");

  async function go() {
    setErr("");
    try {
      await api.unlock(pass);
      onUnlocked();
    } catch (e) {
      setErr(String(e));
    }
  }

  return (
    <div className="panel">
      <h2>Unlock</h2>
      <input
        className="input"
        type="password"
        placeholder="Passphrase"
        value={pass}
        onChange={(e) => setPass(e.target.value)}
        onKeyDown={(e) => e.key === "Enter" && go()}
        autoFocus
      />
      <button className="primary" onClick={go}>
        Unlock
      </button>
      {err && <p className="error">{err}</p>}
    </div>
  );
}
