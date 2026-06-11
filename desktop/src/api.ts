// Thin wrappers over the Tauri IPC commands exposed by src-tauri/src/lib.rs.
// The companion's own UI talks to the core over IPC; external agents/apps use
// the loopback JSON-RPC service instead.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface Status {
  locked: boolean;
  needs_onboarding: boolean;
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

export const api = {
  status: () => invoke<Status>("status"),
  generate: (wordCount: number) => invoke<string>("generate", { wordCount }),
  import: (phrase: string) => invoke<void>("import", { phrase }),
  finalizeSetup: (passphrase: string) => invoke<void>("finalize_setup", { passphrase }),
  unlock: (passphrase: string) => invoke<void>("unlock", { passphrase }),
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
};

/// Subscribe to approval prompts pushed from the service. Returns an unlisten fn.
export function onApprovalRequest(cb: (req: ApprovalRequest) => void): Promise<UnlistenFn> {
  return listen<ApprovalRequest>("approval-request", (e) => cb(e.payload));
}
