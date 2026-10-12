// Thin wrappers over the Tauri IPC commands exposed by src-tauri/src/lib.rs.
// The companion's own UI talks to the core over IPC; external agents/apps use
// the loopback JSON-RPC service instead.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface Status {
  locked: boolean;
  needs_onboarding: boolean;
  /** A vault exists but this build cannot open its version (see #16). */
  vault_unsupported: boolean;
  network: string;
  version: string;
  service_url: string;
  accounts: number;
  node_configured: boolean;
}

export interface Account {
  domain: "user" | "agent";
  index: number;
  address: string;
  label: string;
  owner_client_id?: string;
}

export interface LogEntry {
  ts_unix_ms: number;
  method: string;
  client?: string | null;
  network?: string | null;
  decision: string;
  outcome: string;
  error_code?: number | null;
  latency_ms: number;
  params_json?: string | null;
  result_json?: string | null;
}

export interface ApprovalRequest {
  id: number;
  client_label: string;
  method: string;
  summary: string;
}

export interface Settings {
  sepolia_rpc: string;
  mainnet_rpc: string;
  auto_lock_minutes: number;
}

export interface ClientInfo {
  id: string;
  label: string;
  kind: "agent" | "app";
  granted_until: number | null;
}

// ── On-device proving companion ─────────────────────────────────────────────

export interface ProverStatus {
  prover: string; // backend kind: "native" | "remote"
  ready: boolean;
  version: string;
  service_url: string;
}

export type JobStatus = "queued" | "proving" | "succeeded" | "failed";

export interface Activity {
  seq: number;
  job_id: string;
  status: JobStatus;
  started_at_ms: number;
  label?: string;
}

/** Per-network STRK20 (privacy pool) settings. Empty = default. Keys are secrets. */
export interface Strk20Config {
  /** Pool address override; empty = the network's canonical pool. */
  pool: string;
  /** Starkscan API key with `prove` scope: deposits are proved + screened there (mainnet). */
  starkscan_api_key: string;
  /** A screening `starknet_proveTransaction` endpoint for deposits (overrides the defaults). */
  deposit_prover_url: string;
  /** AVNU paymaster key: relays private transfers/withdrawals. */
  avnu_api_key: string;
  /** Paymaster endpoint override; empty = AVNU's. */
  paymaster_url: string;
}

export interface ProverNetworkConfig {
  rpc_url: string;
  prover_url: string;
  prover_api_key: string;
  strk20: Strk20Config;
}

export interface Strk20Balance {
  token: string;
  /** Hex, base units. */
  balance: string;
}

/** A wallet-API STRK20 action. Amounts are hex felts in base units. */
export type Strk20Action =
  | { type: "deposit"; token: string; amount: string }
  | { type: "transfer"; token: string; amount: string; recipient: string }
  | { type: "withdraw"; token: string; amount: string; recipient: string };

export interface Strk20CallAndProof {
  call: { contract_address: string; entry_point: string; calldata: string[] };
  proof: { data: string; output: string[]; proof_facts: string[] };
}

export interface ProverSettings {
  mainnet: ProverNetworkConfig;
  testnet: ProverNetworkConfig;
  /** "" (env/default), "native", or "remote". Applied on restart. */
  prover_backend: string;
}

export interface StorageStats {
  records: number;
  bytes: number;
}

export interface ProofSummary {
  job_id: string;
  label?: string;
  network: string;
  created_at_ms: number;
  prove_ms: number;
  status: JobStatus;
  proof_bytes: number;
  error?: string;
}

export interface ProofRecord {
  job_id: string;
  label?: string;
  network: string;
  created_at_ms: number;
  prove_ms: number;
  status: JobStatus;
  payload: unknown;
  proof?: { proof?: string; proof_facts?: string[]; l2_to_l1_messages?: unknown[] };
  error?: string;
}

export const api = {
  status: () => invoke<Status>("status"),
  generate: (wordCount: number) => invoke<string>("generate", { wordCount }),
  import: (phrase: string) => invoke<void>("import", { phrase }),
  finalizeSetup: (passphrase: string) => invoke<void>("finalize_setup", { passphrase }),
  unlock: (passphrase: string) => invoke<void>("unlock", { passphrase }),
  /** Move an unopenable vault aside (renames, never deletes); returns its path. */
  archiveUnsupportedVault: () => invoke<string>("archive_unsupported_vault"),
  lock: () => invoke<void>("lock"),
  setNetwork: (network: "mainnet" | "testnet") => invoke<void>("set_network", { network }),
  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (settings: Settings) => invoke<void>("set_settings", { settings }),
  listAccounts: () => invoke<Account[]>("list_accounts"),
  addUserAccount: (label: string) => invoke<Account>("add_user_account", { label }),
  deployStatus: (address: string) => invoke<boolean | null>("deploy_status", { address }),
  // STRK balance in fri (string, to avoid JS precision loss); null when no node.
  balance: (address: string) => invoke<string | null>("balance", { address }),
  deployAccount: (address: string) => invoke<{ transaction_hash: string }>("deploy_account", { address }),
  recentLog: (limit: number) => invoke<LogEntry[]>("recent_log", { limit }),
  listClients: () => invoke<ClientInfo[]>("list_clients"),
  grantPermission: (clientId: string, days: number) =>
    invoke<void>("grant_permission", { clientId, days }),
  revokePermission: (clientId: string) => invoke<void>("revoke_permission", { clientId }),
  respondApproval: (id: number, approved: boolean) =>
    invoke<void>("respond_approval", { id, approved }),

  // Recovery-phrase reveal. IPC only — the loopback service has no equivalent
  // and must never get one. Re-authenticates against the on-disk vault, so a
  // wrong passphrase fails at the AEAD tag rather than a comparison.
  revealSeed: (passphrase: string) => invoke<string>("reveal_seed", { passphrase }),

  // On-device proving companion (IPC-only; agents prove via companion_prove on
  // the loopback service). Settings carry an API key, so they never leave IPC.
  proverStatus: () => invoke<ProverStatus>("prover_status"),
  proofActivity: () => invoke<Activity[]>("proof_activity"),
  getProverSettings: () => invoke<ProverSettings>("get_prover_settings"),
  setProverSettings: (settings: ProverSettings) =>
    invoke<void>("set_prover_settings", { settings }),
  storageStats: () => invoke<StorageStats>("storage_stats"),
  clearStorage: () => invoke<number>("clear_storage"),
  listProofs: () => invoke<ProofSummary[]>("list_proofs"),
  proofDetail: (jobId: string) => invoke<ProofRecord | null>("proof_detail", { jobId }),

  // STRK20 privacy pool. Same handlers as the wallet_strk20* RPC methods; the
  // viewing key never crosses IPC. Errors end with their wallet-API code, e.g.
  // "(118)" = not registered with the pool yet.
  strk20Balances: (address: string, tokens: string[]) =>
    invoke<Strk20Balance[]>("strk20_balances", { address, tokens }),
  strk20Register: (address: string) =>
    invoke<{ transaction_hash: string }>("strk20_register", { address }),
  strk20Invoke: (address: string, actions: Strk20Action[]) =>
    invoke<{ transaction_hash: string }>("strk20_invoke", { address, actions }),
  strk20Prepare: (address: string, actions: Strk20Action[]) =>
    invoke<Strk20CallAndProof>("strk20_prepare", { address, actions }),
};

/// Subscribe to approval prompts pushed from the service. Returns an unlisten fn.
export function onApprovalRequest(cb: (req: ApprovalRequest) => void): Promise<UnlistenFn> {
  return listen<ApprovalRequest>("approval-request", (e) => cb(e.payload));
}
