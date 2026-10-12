import { useEffect, useState } from "react";
import { api, type Account, type Status, type Strk20Action, type Strk20CallAndProof } from "../api";

// STRK has the same address on mainnet and Sepolia.
const STRK = "0x04718f5a0fc34cc1af16a1cdee98ffb20c31f5cd61d6ab07201858f4287c938d";

/// "1.5" STRK → hex base units. Throws on malformed or non-positive input.
function toWeiHex(strk: string): string {
  const m = strk.trim().match(/^(\d+)(?:\.(\d{0,18}))?$/);
  if (!m) throw new Error("enter an amount like 1.5");
  const wei = BigInt(m[1]) * 10n ** 18n + BigInt((m[2] ?? "").padEnd(18, "0") || "0");
  if (wei === 0n) throw new Error("the amount must be positive");
  return "0x" + wei.toString(16);
}

function fmtStrk(hex: string): string {
  const v = BigInt(hex);
  const whole = v / 10n ** 18n;
  const frac = ((v % 10n ** 18n) * 10000n) / 10n ** 18n;
  return `${whole}.${frac.toString().padStart(4, "0")} STRK`;
}

type Kind = "shield" | "send" | "unshield";

/// Private (STRK20) balances: activate an account, shield STRK into the
/// privacy pool, send it privately, unshield it. Proofs run on this device
/// (deposits excepted, see Settings), so actions take about a minute.
export function Privacy({ status }: { status: Status | null }) {
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [address, setAddress] = useState("");
  // undefined = loading, null = not registered with the pool, string = hex balance
  const [balance, setBalance] = useState<string | null | undefined>(undefined);
  const [kind, setKind] = useState<Kind>("shield");
  const [amount, setAmount] = useState("");
  const [recipient, setRecipient] = useState("");
  const [busy, setBusy] = useState("");
  const [err, setErr] = useState("");
  const [done, setDone] = useState("");
  const [prepared, setPrepared] = useState<Strk20CallAndProof | null>(null);

  useEffect(() => {
    api
      .listAccounts()
      .then((a) => {
        setAccounts(a);
        setAddress((cur) => cur || a.find((x) => x.domain === "user")?.address || a[0]?.address || "");
      })
      .catch(() => {});
  }, [status?.network]);

  const refresh = () => {
    if (!address) return;
    setBalance(undefined);
    api
      .strk20Balances(address, [STRK])
      .then((b) => setBalance(b[0]?.balance ?? "0x0"))
      .catch((e) => {
        if (String(e).includes("(118)")) setBalance(null);
        else {
          setBalance("0x0");
          setErr(String(e));
        }
      });
  };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(refresh, [address, status?.network]);

  async function run(label: string, f: () => Promise<unknown>) {
    setErr("");
    setDone("");
    setPrepared(null);
    setBusy(label);
    try {
      await f();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy("");
    }
  }

  const actions = (): Strk20Action[] => {
    const wei = toWeiHex(amount);
    if (kind === "shield") return [{ type: "deposit", token: STRK, amount: wei }];
    if (kind === "send") return [{ type: "transfer", token: STRK, amount: wei, recipient: recipient.trim() }];
    return [{ type: "withdraw", token: STRK, amount: wei, recipient: recipient.trim() || address }];
  };

  const submit = () =>
    run("Proving and submitting… (about a minute)", async () => {
      const r = await api.strk20Invoke(address, actions());
      setDone(
        `Submitted ${r.transaction_hash}. New notes become spendable about 12 blocks after it lands.`,
      );
      setAmount("");
    });

  const prepare = () =>
    run("Proving… (about a minute)", async () => {
      setPrepared(await api.strk20Prepare(address, actions()));
    });

  const needsPaymaster = /AVNU/.test(err);

  return (
    <div className="panel">
      <p className="muted small">
        Private balances live in the Starknet privacy pool (STRK20) on{" "}
        {status?.network === "SN_MAIN" ? "mainnet" : "Sepolia"}. Only this wallet can read or spend
        them; proofs are generated on this device.
      </p>

      <label className="field">
        <span className="muted small">Account</span>
        <select className="input" value={address} onChange={(e) => setAddress(e.target.value)}>
          {accounts.map((a) => (
            <option key={a.address} value={a.address}>
              {a.label} — {a.address.slice(0, 10)}…
            </option>
          ))}
        </select>
      </label>

      {balance === undefined && <p className="muted">Reading the pool…</p>}

      {balance === null && (
        <>
          <p className="muted small">
            This account has not activated private balances. Activating registers its viewing key
            with the pool — public, and it links this address to the pool — and costs one
            transaction from this account.
          </p>
          <button
            className="primary"
            disabled={!!busy}
            onClick={() =>
              run("Proving and submitting… (about a minute)", async () => {
                const r = await api.strk20Register(address);
                setDone(`Activation submitted: ${r.transaction_hash}. Refresh in a minute.`);
              })
            }
          >
            Activate private balance
          </button>
        </>
      )}

      {typeof balance === "string" && (
        <>
          <div className="row">
            <span className="k">Private balance</span>
            <span className="v">{fmtStrk(balance)}</span>
            <button className="small-btn" onClick={refresh} disabled={!!busy}>
              Refresh
            </button>
          </div>

          <div className="subtabs">
            {(["shield", "send", "unshield"] as const).map((k) => (
              <button
                key={k}
                className={kind === k ? "subtab active" : "subtab"}
                onClick={() => {
                  setKind(k);
                  setErr("");
                }}
              >
                {k === "shield" ? "Shield" : k === "send" ? "Send privately" : "Unshield"}
              </button>
            ))}
          </div>
          <p className="muted small">
            {kind === "shield" &&
              "Moves public STRK from this account into the pool. Deposits are public and screened by a remote prover (see Settings)."}
            {kind === "send" &&
              "Sends shielded STRK to another registered account. Relayed by AVNU if you set a key; otherwise prepare it and submit from another account."}
            {kind === "unshield" &&
              "Withdraws shielded STRK to a public address (this account by default)."}
          </p>
          <input
            className="input"
            placeholder="Amount in STRK, e.g. 1.5"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
          />
          {kind !== "shield" && (
            <input
              className="input"
              placeholder={kind === "send" ? "Recipient address" : `Recipient (default: this account)`}
              value={recipient}
              onChange={(e) => setRecipient(e.target.value)}
            />
          )}
          <div className="addrow">
            <button className="primary" disabled={!!busy || !amount} onClick={submit}>
              {kind === "shield" ? "Shield" : kind === "send" ? "Send" : "Unshield"}
            </button>
            {kind !== "shield" && (
              <button className="small-btn" disabled={!!busy || !amount} onClick={prepare}>
                Prepare only
              </button>
            )}
          </div>
        </>
      )}

      {busy && <p className="muted">{busy}</p>}
      {done && <p className="muted small">{done}</p>}
      {err && <p className="error">{err}</p>}
      {needsPaymaster && (
        <p className="muted small">
          Tip: use “Prepare only”, then submit the call and proof from any other account (an agent
          can do it with wallet_addInvokeTransaction).
        </p>
      )}
      {prepared && (
        <>
          <p className="muted small">
            Proved, not submitted. Submit <code>call</code> as a proof-carrying invoke from any
            account, with <code>proof.proof_facts</code> and <code>proof.data</code>.
          </p>
          <textarea className="input" readOnly rows={8} value={JSON.stringify(prepared, null, 2)} />
          <button className="small-btn" onClick={() => navigator.clipboard.writeText(JSON.stringify(prepared))}>
            Copy
          </button>
        </>
      )}
    </div>
  );
}
