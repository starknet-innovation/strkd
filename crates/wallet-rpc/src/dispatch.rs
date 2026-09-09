//! Request dispatch: the wallet's brain. Pure-ish and fully testable without
//! any HTTP — `dispatch()` takes an authenticated-or-not token and a parsed
//! request and returns a response.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use serde_json::{json, Value};
use tokio::sync::Mutex;
use wallet_core::{address_hex, AccountRef, ChainId, Felt};

use wallet_core::{Call, InvokeV3Params, ResourceBounds, SierraClass};

use crate::approval::{ApprovalRequest, Approver, Decision};
use crate::auth::{ClientKind, ClientStore, PairedClient};
use crate::error::WalletRpcError;
use crate::jsonrpc::{Request, Response};
use crate::log::{now_unix_ms, LogEntry, RequestLog};
use crate::node::{FeeBounds, StarknetRpc};
use crate::session::WalletSession;
use crate::store::VaultStore;

/// Shared state behind the service.
pub struct ServerState {
    pub session: Arc<Mutex<WalletSession>>,
    pub clients: Arc<Mutex<ClientStore>>,
    pub approver: Arc<dyn Approver>,
    pub log: Arc<Mutex<RequestLog>>,
    /// On-disk vault, if persistence is configured. When set, registry changes
    /// (e.g. agent-account creation) are re-sealed and written back.
    pub vault_store: Option<VaultStore>,
    /// Starknet node clients **per network**, so switching chains also switches
    /// the RPC endpoint (no cross-network broadcasts). Swappable at runtime from
    /// the settings control panel, hence behind a lock.
    nodes: RwLock<HashMap<ChainId, Arc<dyn StarknetRpc>>>,
    /// Tokens the user has chosen to watch (`wallet_watchAsset`). In-memory,
    /// display-only this phase.
    watched_assets: RwLock<Vec<Value>>,
    /// STRK20 (Tongo) pool registry per network: ERC-20 token → pool contract.
    /// A Tongo contract wraps exactly one token, so this is the wallet's whole
    /// notion of "which shielded tokens exist". Configured by the human (or
    /// the embedding app); an unknown token errors with NOT_REGISTERED (118).
    strk20_pools: RwLock<HashMap<(ChainId, Felt), Felt>>,
    /// Unix-ms of the last meaningful activity (an RPC request, or an unlock).
    /// The app's auto-lock task locks the wallet after this goes stale. Desktop
    /// status-polling (IPC, not RPC) deliberately does NOT touch it.
    last_activity_ms: AtomicU64,
    /// GitHub repo (`owner/name`) that `companion_reportIssue` points its
    /// prefilled "new issue" links at. Not a secret; no token is involved.
    issue_repo: String,
    /// On-device proving companion, if wired. When set, the `companion_prove*`
    /// methods generate proofs locally from an **already-signed** payload. The
    /// prover holds no key material; its per-network settings (which carry a
    /// remote-prover API key) are IPC-only and never reachable through here.
    pub prover: Option<Arc<prover::ProverState>>,
    pub api_version: String,
    pub spec_versions: Vec<String>,
}

impl ServerState {
    /// Construct with a default in-memory log (full payloads on — the debug
    /// default per spec §9). Use [`ServerState::with_log`] to supply a
    /// file-backed log for production.
    pub fn new(session: Arc<Mutex<WalletSession>>, approver: Arc<dyn Approver>) -> Self {
        let log = RequestLog::in_memory(true).expect("open in-memory request log");
        Self::with_log(session, approver, log)
    }

    /// Construct with a caller-provided request log (e.g. file-backed).
    pub fn with_log(
        session: Arc<Mutex<WalletSession>>,
        approver: Arc<dyn Approver>,
        log: RequestLog,
    ) -> Self {
        ServerState {
            session,
            clients: Arc::new(Mutex::new(ClientStore::new())),
            approver,
            log: Arc::new(Mutex::new(log)),
            vault_store: None,
            nodes: RwLock::new(HashMap::new()),
            watched_assets: RwLock::new(Vec::new()),
            strk20_pools: RwLock::new(HashMap::new()),
            last_activity_ms: AtomicU64::new(now_unix_ms()),
            issue_repo: DEFAULT_ISSUE_REPO.to_string(),
            prover: None,
            api_version: "0.1.0".to_string(),
            // The Starknet JSON-RPC spec the wallet's node client targets.
            // Verified against the Sepolia node (starknet_specVersion → 0.10.2,
            // 2026-06-23); the node HTTP client / V3 tx wire format track v0.10.
            spec_versions: vec!["0.10.2".to_string()],
        }
    }

    /// Override the GitHub repo (`owner/name`) that `companion_reportIssue`
    /// targets. Ignored (keeps the default) unless `repo` is a plain
    /// `owner/name` slug, so we never build a malformed/abusable URL.
    pub fn with_issue_repo(mut self, repo: impl Into<String>) -> Self {
        let repo = repo.into();
        if is_valid_repo_slug(&repo) {
            self.issue_repo = repo;
        }
        self
    }

    /// The GitHub repo (`owner/name`) feedback links target.
    pub fn issue_repo(&self) -> &str {
        &self.issue_repo
    }

    /// Attach an on-disk vault store so registry changes are persisted.
    pub fn with_vault_store(mut self, store: VaultStore) -> Self {
        self.vault_store = Some(store);
        self
    }

    /// Use a file-backed client store (persists pairings + grants across
    /// restarts). Loads any existing clients from `path`.
    pub fn with_clients_path(mut self, path: std::path::PathBuf) -> Self {
        self.clients = Arc::new(Mutex::new(ClientStore::open(path)));
        self
    }

    /// Attach a node client for `chain` at construction.
    pub fn with_node(self, chain: ChainId, node: Arc<dyn StarknetRpc>) -> Self {
        self.set_node(chain, Some(node));
        self
    }

    /// Attach the on-device proving companion so `companion_prove*` is available.
    pub fn with_prover(mut self, prover: Arc<prover::ProverState>) -> Self {
        self.prover = Some(prover);
        self
    }

    /// Set or clear the node client for `chain` at runtime (settings panel).
    pub fn set_node(&self, chain: ChainId, node: Option<Arc<dyn StarknetRpc>>) {
        let mut nodes = self.nodes.write().unwrap();
        match node {
            Some(n) => {
                nodes.insert(chain, n);
            }
            None => {
                nodes.remove(&chain);
            }
        }
    }

    /// Whether a node is configured for `chain`.
    pub fn has_node_for(&self, chain: ChainId) -> bool {
        self.nodes.read().unwrap().contains_key(&chain)
    }

    /// A cloned handle to `chain`'s node, if any (never held across an await).
    pub fn node_for(&self, chain: ChainId) -> Option<Arc<dyn StarknetRpc>> {
        self.nodes.read().unwrap().get(&chain).cloned()
    }

    /// Register (or with `None`, remove) the STRK20 privacy pool wrapping
    /// `token` on `chain`. Builder-style twin: [`ServerState::with_strk20_pool`].
    pub fn set_strk20_pool(&self, chain: ChainId, token: Felt, pool: Option<Felt>) {
        let mut pools = self.strk20_pools.write().unwrap();
        match pool {
            Some(p) => {
                pools.insert((chain, token), p);
            }
            None => {
                pools.remove(&(chain, token));
            }
        }
    }

    /// Register a STRK20 pool at construction.
    pub fn with_strk20_pool(self, chain: ChainId, token: Felt, pool: Felt) -> Self {
        self.set_strk20_pool(chain, token, Some(pool));
        self
    }

    /// The pool wrapping `token` on `chain`, if registered.
    pub fn strk20_pool_for(&self, chain: ChainId, token: &Felt) -> Option<Felt> {
        self.strk20_pools.read().unwrap().get(&(chain, *token)).copied()
    }

    /// All tokens with a registered pool on `chain` (the wallet's "all
    /// shielded tokens" set for `wallet_strk20Balances` with `tokens: []`).
    pub fn strk20_tokens(&self, chain: ChainId) -> Vec<Felt> {
        let mut v: Vec<Felt> = self
            .strk20_pools
            .read()
            .unwrap()
            .keys()
            .filter(|(c, _)| *c == chain)
            .map(|(_, t)| *t)
            .collect();
        v.sort();
        v
    }

    /// Mark "now" as the last activity (resets the auto-lock idle timer).
    pub fn touch_activity(&self) {
        self.last_activity_ms.store(now_unix_ms(), Ordering::Relaxed);
    }

    /// Unix-ms of the last activity (for the auto-lock task).
    pub fn last_activity_ms(&self) -> u64 {
        self.last_activity_ms.load(Ordering::Relaxed)
    }

    /// Record a watched asset (display-only).
    pub fn add_watched_asset(&self, asset: Value) {
        self.watched_assets.write().unwrap().push(asset);
    }

    /// Snapshot of watched assets.
    pub fn watched_assets(&self) -> Vec<Value> {
        self.watched_assets.read().unwrap().clone()
    }
}

/// STRK fee-token contract address. At the time of writing this is the same on
/// Starknet mainnet and Sepolia, but it is **not** treated as authoritative:
/// verify against the target network, and callers may override via the `token`
/// param of `companion_requestFunding`.
pub const STRK_TOKEN_ADDRESS: &str =
    "0x04718f5a0fc34cc1af16a1cdee98ffb20c31f5cd61d6ab07201858f4287c938d";

/// Default GitHub repo (`owner/name`) that `companion_reportIssue` builds its
/// prefilled "new issue" links against. Overridable via
/// [`ServerState::with_issue_repo`].
const DEFAULT_ISSUE_REPO: &str = "starknet-innovation/strkd";

/// `owner/name` with only GitHub-legal slug characters, exactly one `/`.
fn is_valid_repo_slug(s: &str) -> bool {
    let mut parts = s.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(name), None) => {
            !owner.is_empty()
                && !name.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
        }
        _ => false,
    }
}

/// Percent-encode a string for use as a URL query-component value (RFC 3986:
/// keep unreserved chars, `%XX` everything else — spaces → `%20`, newlines →
/// `%0A`). Keeps agent-authored text from breaking out of the `?title=&body=`
/// query.
fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Truncate to at most `max` chars (char-safe), appending `…` if cut.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

/// Append a `**Label:** value` markdown line to `body` if `val` is non-blank.
fn push_field(body: &mut String, label: &str, val: &str) {
    let v = val.trim();
    if !v.is_empty() {
        body.push_str("**");
        body.push_str(label);
        body.push_str(":** ");
        body.push_str(v);
        body.push_str("\n\n");
    }
}

pub(crate) fn felt_hex(f: &Felt) -> String {
    format!("0x{:x}", f)
}

pub(crate) fn parse_u128_str(s: &str) -> Result<u128, WalletRpcError> {
    let s = s.trim();
    let parsed = if let Some(h) = s.strip_prefix("0x") {
        u128::from_str_radix(h, 16)
    } else {
        s.parse::<u128>()
    };
    parsed.map_err(|_| WalletRpcError::InvalidRequest(format!("invalid u128 value '{s}'")))
}

fn parse_u128_field(params: &Value, key: &str) -> Result<u128, WalletRpcError> {
    match params.get(key) {
        Some(Value::String(s)) => parse_u128_str(s),
        Some(Value::Number(n)) => n
            .as_u64()
            .map(u128::from)
            .ok_or_else(|| WalletRpcError::InvalidRequest(format!("bad {key}"))),
        _ => Err(WalletRpcError::InvalidRequest(format!("missing '{key}'"))),
    }
}

pub(crate) fn chain_name(c: ChainId) -> &'static str {
    match c {
        ChainId::Mainnet => "SN_MAIN",
        ChainId::Sepolia => "SN_SEPOLIA",
    }
}

fn param_str(params: &Value, key: &str) -> Result<String, WalletRpcError> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| WalletRpcError::InvalidRequest(format!("missing string param '{key}'")))
}

pub(crate) fn opt_param_str(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

/// Which accounts this client may see/use.
pub(crate) fn scope_for(client: &PairedClient) -> Option<String> {
    match client.kind {
        ClientKind::Agent => Some(client.id.clone()),
        ClientKind::App => None,
    }
}

fn account_to_json(a: &AccountRef) -> Value {
    json!({
        "domain": a.domain,
        "index": a.index,
        "address": a.address,
        "label": a.label,
    })
}

/// Outcome of handling, plus what to record in the log.
struct Handled {
    result: Result<Value, WalletRpcError>,
    decision: String,
    client: Option<String>,
}

/// Standard methods that exist in the spec but are not implemented.
/// `wallet_strk20SubaccountCommitment` (spec 0.10.4) needs a note-based pool
/// with sub-account identity keys — not expressible in the Tongo backend.
fn is_deferred(method: &str) -> bool {
    matches!(
        method,
        "wallet_addStarknetChain" | "wallet_strk20SubaccountCommitment"
    )
}

pub async fn dispatch(state: &ServerState, token: Option<&str>, req: Request) -> Response {
    let start = std::time::Instant::now();
    // An RPC request counts as activity (keeps a busy agent's session unlocked);
    // the desktop's status polling goes through IPC, not here, so it doesn't.
    state.touch_activity();
    let id = req.id.clone();
    let method = req.method.clone();
    // Params are public (no secrets pass through params; tokens ride the header
    // and are never logged). Captured for the full-payload log; redaction is
    // applied in RequestLog when the toggle is off.
    let params_json = serde_json::to_string(&req.params).ok();

    let handled = handle(state, token, req).await;

    let (outcome, error_code, result_json) = match &handled.result {
        Ok(v) => ("ok".to_string(), None, serde_json::to_string(v).ok()),
        Err(e) => (
            format!("error {}: {}", e.code(), e.message()),
            Some(e.code()),
            None,
        ),
    };
    let network = Some(chain_name(state.session.lock().await.chain()).to_string());

    {
        let log = state.log.lock().await;
        log.record(LogEntry {
            ts_unix_ms: now_unix_ms(),
            method,
            client: handled.client,
            network,
            decision: handled.decision,
            outcome,
            error_code,
            latency_ms: start.elapsed().as_millis() as u64,
            params_json,
            result_json,
        });
    }

    match handled.result {
        Ok(value) => Response::ok(id, value),
        Err(e) => Response::err(id, e.to_object()),
    }
}

async fn handle(state: &ServerState, token: Option<&str>, req: Request) -> Handled {
    let params = req.params;
    let method = req.method.as_str();

    // --- Public, unauthenticated methods -------------------------------------
    match method {
        "wallet_supportedWalletApi" => {
            return Handled {
                result: Ok(json!([state.api_version])),
                decision: "n/a".into(),
                client: None,
            };
        }
        "wallet_supportedSpecs" => {
            return Handled {
                result: Ok(json!(state.spec_versions)),
                decision: "n/a".into(),
                client: None,
            };
        }
        "wallet_getPermissions" => {
            // Returns granted permissions for the caller, or [] if not paired.
            let granted = match token {
                Some(t) if state.clients.lock().await.verify(t).is_some() => json!(["accounts"]),
                _ => json!([]),
            };
            return Handled {
                result: Ok(granted),
                decision: "n/a".into(),
                client: None,
            };
        }
        "companion_getStatus" => {
            // If a valid token is presented, include this client's grant state so
            // it knows whether to expect prompts (else `grant: null`).
            let grant = match verify_token(state, token).await {
                Some(c) => json!({
                    "active": c.grant_active(now_unix_ms()),
                    "expires_at": c.granted_until,
                }),
                None => Value::Null,
            };
            let session = state.session.lock().await;
            return Handled {
                result: Ok(json!({
                    "locked": session.is_locked(),
                    "network": chain_name(session.chain()),
                    "api_version": state.api_version,
                    "grant": grant,
                })),
                decision: "n/a".into(),
                client: None,
            };
        }
        "companion_requestPairing" => return handle_pairing(state, &params).await,
        _ => {}
    }

    // --- Everything else requires a paired token -----------------------------
    let client = match verify_token(state, token).await {
        Some(c) => c,
        None => {
            return Handled {
                result: Err(WalletRpcError::NotRegistered),
                decision: "n/a".into(),
                client: None,
            }
        }
    };
    let client_label = format!("{} ({})", client.label, client.id);

    if is_deferred(method) {
        return Handled {
            result: Err(WalletRpcError::NotImplemented(format!(
                "{method} is planned for a later phase"
            ))),
            decision: "n/a".into(),
            client: Some(client_label),
        };
    }

    let result = match method {
        "wallet_requestChainId" => {
            let session = state.session.lock().await;
            Ok(Value::String(felt_hex(&session.chain().as_felt())))
        }
        "wallet_requestAccounts" | "companion_listAccounts" => {
            let detailed = method == "companion_listAccounts";
            let session = state.session.lock().await;
            match session.registry() {
                Ok(reg) => {
                    let scope = scope_for(&client);
                    let items: Vec<Value> = reg
                        .scoped_for(scope.as_deref())
                        .map(|a| {
                            if detailed {
                                account_to_json(a)
                            } else {
                                Value::String(a.address.clone())
                            }
                        })
                        .collect();
                    Ok(json!(items))
                }
                Err(e) => Err(e),
            }
        }
        "wallet_deploymentData" => handle_deployment_data(state, &client).await,
        "wallet_signTypedData" => handle_sign_typed_data(state, &client, &params).await,
        "wallet_addInvokeTransaction" => handle_add_invoke(state, &client, &params).await,
        "wallet_addDeclareTransaction" => handle_add_declare(state, &client, &params).await,
        "companion_createAgentAccount" => handle_create_agent_account(state, &client, &params).await,
        "wallet_switchStarknetChain" => handle_switch_chain(state, &client, &params).await,
        "wallet_watchAsset" => handle_watch_asset(state, &client, &params).await,
        "companion_fundingSource" => handle_funding_source(state).await,
        "companion_reportIssue" => handle_report_issue(state, &client, &params),
        "companion_typedDataHash" => handle_typed_data_hash(&params),
        "companion_estimateFee" => handle_estimate_fee(state, &client, &params).await,
        "companion_requestGrant" => handle_request_grant(state, &client, &params).await,
        "companion_requestFunding" => handle_request_funding(state, &client, &params).await,
        "companion_deployAccount" => handle_deploy_account(state, &client, &params).await,
        "companion_prove" => handle_prove(state, &params).await,
        "companion_proveStatus" => handle_prove_status(state, &params).await,
        "companion_proofActivity" => handle_proof_activity(state).await,
        "companion_signAndProve" => handle_sign_and_prove(state, &client, &params).await,
        "wallet_strk20Balances" => crate::strk20::handle_balances(state, &client, &params).await,
        "wallet_strk20PrepareInvoke" => {
            crate::strk20::handle_prepare_invoke(state, &client, &params).await
        }
        "wallet_strk20InvokeTransaction" => {
            crate::strk20::handle_invoke_transaction(state, &client, &params).await
        }
        _ => Err(WalletRpcError::NotImplemented(format!(
            "unknown method '{method}'"
        ))),
    };

    // Decision label: approval handlers report their own; others are "n/a".
    let decision = match method {
        "wallet_signTypedData"
        | "wallet_addInvokeTransaction"
        | "wallet_addDeclareTransaction"
        | "wallet_switchStarknetChain"
        | "wallet_watchAsset"
        | "companion_createAgentAccount"
        | "companion_requestFunding"
        | "companion_requestGrant"
        | "companion_deployAccount"
        | "companion_signAndProve"
        | "wallet_strk20PrepareInvoke"
        | "wallet_strk20InvokeTransaction" => decision_label(&result),
        _ => "n/a".into(),
    };

    Handled {
        result,
        decision,
        client: Some(client_label),
    }
}

/// Approval that honors an active permission grant: while a client's grant is
/// active, its **own-account** operations auto-approve (no menu-bar prompt).
/// Funding, pairing, and wallet-config changes never call this — they always
/// prompt (funding spends the user's manager account; the rest change shared
/// state). The agent is independently scoped to its own accounts, so a grant's
/// blast radius is limited to accounts the agent controls.
pub(crate) async fn gated_approval(
    state: &ServerState,
    client: &PairedClient,
    method: &str,
    summary: String,
) -> Decision {
    if client.grant_active(now_unix_ms()) {
        return Decision::Approve;
    }
    state
        .approver
        .request_approval(ApprovalRequest {
            client_label: format!("{} ({})", client.label, client.id),
            method: method.to_string(),
            summary,
        })
        .await
}

/// "rejected" when the result is a UserRefused error, else "approved"/"n/a".
fn decision_label(result: &Result<Value, WalletRpcError>) -> String {
    match result {
        Err(WalletRpcError::UserRefused) => "rejected".into(),
        Ok(_) => "approved".into(),
        Err(_) => "n/a".into(),
    }
}

async fn verify_token(state: &ServerState, token: Option<&str>) -> Option<PairedClient> {
    let t = token?;
    state.clients.lock().await.verify(t)
}

async fn handle_pairing(state: &ServerState, params: &Value) -> Handled {
    let name = opt_param_str(params, "name").unwrap_or_else(|| "unnamed client".into());
    let kind = match opt_param_str(params, "kind").as_deref() {
        Some("agent") => ClientKind::Agent,
        _ => ClientKind::App,
    };
    let reattach = params.get("reattach").and_then(|v| v.as_bool()) == Some(true);

    // Re-attach: if a client with this (name, kind) already exists, issue it a
    // fresh token (keeping its id + accounts) instead of stranding them under a
    // new client_id. Human-approved, with the consequence shown.
    let existing_id = if reattach {
        state.clients.lock().await.find_id(&name, kind)
    } else {
        None
    };

    if let Some(id) = existing_id {
        let n_accounts = {
            let session = state.session.lock().await;
            session
                .registry()
                .map(|r| r.scoped_for(Some(&id)).count())
                .unwrap_or(0)
        };
        let decision = state
            .approver
            .request_approval(ApprovalRequest {
                client_label: name.clone(),
                method: "companion_requestPairing".into(),
                summary: format!(
                    "Re-attach {:?} client \"{name}\" ({id}) — the caller will control {n_accounts} existing account(s)",
                    kind
                ),
            })
            .await;
        if decision == Decision::Reject {
            return Handled {
                result: Err(WalletRpcError::UserRefused),
                decision: "rejected".into(),
                client: Some(name),
            };
        }
        let (id, tok) = state
            .clients
            .lock()
            .await
            .reattach(&name, kind)
            .expect("client existed");
        return Handled {
            result: Ok(json!({ "client_id": id, "token": tok, "reattached": true })),
            decision: "approved".into(),
            client: Some(name),
        };
    }

    let decision = state
        .approver
        .request_approval(ApprovalRequest {
            client_label: name.clone(),
            method: "companion_requestPairing".into(),
            summary: format!("Pair new {:?} client \"{name}\"", kind),
        })
        .await;

    if decision == Decision::Reject {
        return Handled {
            result: Err(WalletRpcError::UserRefused),
            decision: "rejected".into(),
            client: Some(name),
        };
    }

    let (id, tok) = state.clients.lock().await.pair(name.clone(), kind);
    Handled {
        result: Ok(json!({ "client_id": id, "token": tok })),
        decision: "approved".into(),
        client: Some(name),
    }
}

async fn handle_deployment_data(
    state: &ServerState,
    client: &PairedClient,
) -> Result<Value, WalletRpcError> {
    // Pick the first in-scope account.
    let account = {
        let session = state.session.lock().await;
        let reg = session.registry()?;
        let found = reg
            .scoped_for(scope_for(client).as_deref())
            .next()
            .cloned()
            .ok_or(WalletRpcError::DeploymentDataNotAvailable)?;
        found
    };
    let session = state.session.lock().await;
    let d = session.deployment_data_for(&account)?;
    Ok(json!({
        "address": address_hex(&d.address),
        "class_hash": felt_hex(&d.class_hash),
        "salt": felt_hex(&d.salt),
        "constructor_calldata": d.constructor_calldata.iter().map(felt_hex).collect::<Vec<_>>(),
    }))
}

async fn handle_sign_typed_data(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let account_address = param_str(params, "account_address")?;
    let want = normalize_address(&account_address)?;

    let td_value = params
        .get("typed_data")
        .ok_or_else(|| WalletRpcError::InvalidRequest("missing 'typed_data'".into()))?;
    let typed_data_json = match td_value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other)
            .map_err(|_| WalletRpcError::InvalidRequest("typed_data not serializable".into()))?,
    };

    // Resolve the account within the caller's scope (enforces that a client can
    // only sign with accounts it is allowed to use). Done under a brief lock; the
    // lock is released before we await approval.
    let account = {
        let session = state.session.lock().await;
        let reg = session.registry()?;
        let found = reg
            .scoped_for(scope_for(client).as_deref())
            .find(|a| a.address == want)
            .cloned()
            .ok_or(WalletRpcError::Forbidden)?;
        found
    };

    let decision = gated_approval(
        state,
        client,
        "wallet_signTypedData",
        format!("Sign typed data with account {}", account.address),
    )
    .await;
    if decision == Decision::Reject {
        return Err(WalletRpcError::UserRefused);
    }

    let session = state.session.lock().await;
    let sig = session.sign_typed_data_for(&account, &typed_data_json)?;
    Ok(json!([felt_hex(&sig.r), felt_hex(&sig.s)]))
}

/// `companion_typedDataHash` — compute the SNIP-12 (revision 1) message hash
/// that `wallet_signTypedData` would sign for `{ account_address, typed_data }`.
///
/// Pure and key-free: `wallet_signTypedData` returns only the spec `[r, s]`
/// signature, so this lets a caller confirm strkd hashes a typed message the
/// same way starknet.js `typedData.getMessageHash` does — and thus that a Cairo
/// account's `is_valid_signature` will accept the resulting signature — without
/// signing anything. Needs no unlock and prompts no approval.
fn handle_typed_data_hash(params: &Value) -> Result<Value, WalletRpcError> {
    let account_address = param_str(params, "account_address")?;
    let account = Felt::from_hex(&account_address)
        .map_err(|_| WalletRpcError::InvalidRequest("invalid 'account_address'".into()))?;

    let td_value = params
        .get("typed_data")
        .ok_or_else(|| WalletRpcError::InvalidRequest("missing 'typed_data'".into()))?;
    let typed_data_json = match td_value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other)
            .map_err(|_| WalletRpcError::InvalidRequest("typed_data not serializable".into()))?,
    };

    let hash = wallet_core::typed_data_message_hash(&typed_data_json, &account)?;
    Ok(json!({ "hash": felt_hex(&hash), "revision": "1" }))
}

fn felt_from(value: &Value, what: &str) -> Result<Felt, WalletRpcError> {
    let s = value
        .as_str()
        .ok_or_else(|| WalletRpcError::InvalidRequest(format!("{what} must be a hex string")))?;
    Felt::from_hex(s).map_err(|_| WalletRpcError::InvalidRequest(format!("invalid felt for {what}")))
}

fn parse_resource_bounds(v: &Value) -> Result<ResourceBounds, WalletRpcError> {
    // Accept { max_amount, max_price_per_unit } as decimal or 0x-hex strings/numbers.
    let parse_u128 = |field: &str| -> Result<u128, WalletRpcError> {
        match v.get(field) {
            Some(Value::String(s)) => {
                let s = s.trim();
                if let Some(h) = s.strip_prefix("0x") {
                    u128::from_str_radix(h, 16)
                } else {
                    s.parse::<u128>()
                }
                .map_err(|_| WalletRpcError::InvalidRequest(format!("bad {field}")))
            }
            Some(Value::Number(n)) => n
                .as_u64()
                .map(u128::from)
                .ok_or_else(|| WalletRpcError::InvalidRequest(format!("bad {field}"))),
            _ => Ok(0),
        }
    };
    Ok(ResourceBounds {
        max_amount: parse_u128("max_amount")? as u64,
        max_price_per_unit: parse_u128("max_price_per_unit")?,
    })
}

fn parse_calls(params: &Value) -> Result<Vec<Call>, WalletRpcError> {
    let arr = params
        .get("calls")
        .and_then(|v| v.as_array())
        .ok_or_else(|| WalletRpcError::InvalidRequest("missing 'calls' array".into()))?;
    if arr.is_empty() {
        return Err(WalletRpcError::InvalidRequest("'calls' is empty".into()));
    }
    let mut out = Vec::with_capacity(arr.len());
    for c in arr {
        // Accept common aliases for the contract address.
        let to_val = c
            .get("contract_address")
            .or_else(|| c.get("contractAddress"))
            .or_else(|| c.get("to"))
            .ok_or_else(|| {
                WalletRpcError::InvalidRequest("call missing contract_address".into())
            })?;
        let to = felt_from(to_val, "contract_address")?;
        // Accept the standard `entry_point_selector` or the common `entrypoint` /
        // `entry_point` / `selector`. The value may be a function NAME or a 0x
        // selector (resolve_selector handles both).
        let selector_field = c
            .get("entry_point_selector")
            .or_else(|| c.get("entrypoint"))
            .or_else(|| c.get("entry_point"))
            .or_else(|| c.get("selector"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                WalletRpcError::InvalidRequest(
                    "call missing entry_point_selector (or entrypoint) — a function name or 0x selector".into(),
                )
            })?;
        let selector = wallet_core::resolve_selector(selector_field)
            .map_err(|_| WalletRpcError::InvalidRequest("bad entry_point_selector".into()))?;
        let calldata = match c.get("calldata") {
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| felt_from(v, "calldata element"))
                .collect::<Result<Vec<_>, _>>()?,
            None | Some(Value::Null) => Vec::new(),
            _ => return Err(WalletRpcError::InvalidRequest("calldata must be an array".into())),
        };
        out.push(Call {
            to,
            selector,
            calldata,
        });
    }
    Ok(out)
}

/// Optional caller-supplied nonce.
fn opt_nonce(params: &Value) -> Result<Option<Felt>, WalletRpcError> {
    match params.get("nonce") {
        Some(Value::Null) | None => Ok(None),
        Some(v) => Ok(Some(felt_from(v, "nonce")?)),
    }
}

/// Optional caller-supplied resource bounds (`{ l1_gas, l2_gas, l1_data_gas }`).
fn opt_fee_bounds(params: &Value) -> Result<Option<FeeBounds>, WalletRpcError> {
    let rb = match params.get("resource_bounds") {
        Some(Value::Null) | None => return Ok(None),
        Some(v) => v,
    };
    let get = |k: &str| -> Result<ResourceBounds, WalletRpcError> {
        rb.get(k)
            .ok_or_else(|| WalletRpcError::InvalidRequest(format!("resource_bounds missing '{k}'")))
            .and_then(parse_resource_bounds)
    };
    Ok(Some(FeeBounds {
        l1_gas: get("l1_gas")?,
        l2_gas: get("l2_gas")?,
        l1_data_gas: get("l1_data_gas")?,
    }))
}

/// Resolve the chain for an operation: a per-request `chainId` (felt-encoded,
/// e.g. `SN_SEPOLIA` / `SN_MAIN`) wins; otherwise fall back to the wallet's
/// active default chain (set by the human in the desktop Settings). Per-request
/// `chainId` is the recommended way for an agent to pick a network — it's
/// explicit and avoids the cross-agent race of a single shared mutable active
/// chain (one client switching the network out from under another). An
/// unsupported chain → 117; a malformed value → 114.
pub(crate) async fn resolve_chain(state: &ServerState, params: &Value) -> Result<ChainId, WalletRpcError> {
    match params.get("chainId") {
        None | Some(Value::Null) => Ok(state.session.lock().await.chain()),
        Some(Value::String(s)) => {
            let felt = Felt::from_hex(s).map_err(|_| {
                WalletRpcError::InvalidRequest(
                    "invalid chainId (expected a felt-encoded chain id, e.g. SN_SEPOLIA / SN_MAIN)"
                        .into(),
                )
            })?;
            ChainId::from_felt(&felt).map_err(|_| WalletRpcError::ChainIdNotSupported)
        }
        Some(_) => Err(WalletRpcError::InvalidRequest(
            "chainId must be a felt-encoded string".into(),
        )),
    }
}

/// Resolve the nonce + fee bounds for an invoke: caller-supplied values win,
/// otherwise fetch the nonce and estimate the fee from the node. Errors with
/// `NoNode` if a value is missing and no node is configured.
///
/// `proof_carrying` must be set for SNIP-36 proof-carrying invokes (non-empty
/// `proof_facts`). Online fee estimation simulates the call *without*
/// `proof_facts` in `tx_info`, so a contract that reads them (e.g.
/// `proof_facts.at(8)`) reverts during estimation — and even if it didn't, the
/// estimate would be for the wrong execution. Such invokes therefore require
/// explicit `resource_bounds` rather than auto-estimation.
pub(crate) async fn resolve_exec(
    state: &ServerState,
    chain: ChainId,
    sender: &Felt,
    encoded_calldata: &[Felt],
    params: &Value,
    proof_carrying: bool,
) -> Result<(Felt, FeeBounds), WalletRpcError> {
    let nonce = match opt_nonce(params)? {
        Some(n) => n,
        None => {
            let node = state.node_for(chain).ok_or(WalletRpcError::NoNode)?;
            node.get_nonce(sender)
                .await
                .map_err(|e| WalletRpcError::Node(e.to_string()))?
        }
    };
    let bounds = match opt_fee_bounds(params)? {
        Some(b) => b,
        None if proof_carrying => {
            return Err(WalletRpcError::InvalidRequest(
                "proof-carrying invoke (proof_facts) requires explicit resource_bounds: \
                 online fee estimation simulates the call without proof_facts and the \
                 contract reverts reading them. Set resource_bounds manually (e.g. ~2× \
                 current gas prices)."
                    .into(),
            ));
        }
        None => {
            let node = state.node_for(chain).ok_or(WalletRpcError::NoNode)?;
            node.estimate_invoke(sender, encoded_calldata, &nonce)
                .await
                .map_err(|e| WalletRpcError::Node(e.to_string()))?
        }
    };
    Ok((nonce, bounds))
}

pub(crate) fn bounds_summary(b: &FeeBounds) -> String {
    format!(
        "l1 {}×{} / l2 {}×{} / l1_data {}×{}",
        b.l1_gas.max_amount,
        b.l1_gas.max_price_per_unit,
        b.l2_gas.max_amount,
        b.l2_gas.max_price_per_unit,
        b.l1_data_gas.max_amount,
        b.l1_data_gas.max_price_per_unit,
    )
}

/// Sign the invoke and, if `submit`, broadcast it via the node. Returns the
/// JSON-RPC result. Caller must have already obtained approval.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn sign_and_submit(
    state: &ServerState,
    chain: ChainId,
    account: &AccountRef,
    calls: &[Call],
    nonce: Felt,
    bounds: FeeBounds,
    submit: bool,
    proof_facts: Vec<Felt>,
    proof: Option<String>,
) -> Result<Value, WalletRpcError> {
    // proof_facts (when non-empty) extends the V3 hash (SNIP-36), so the
    // signature covers them.
    let params_v3 = InvokeV3Params {
        nonce,
        tip: 0,
        l1_gas: bounds.l1_gas,
        l2_gas: bounds.l2_gas,
        l1_data_gas: bounds.l1_data_gas,
        proof_facts: proof_facts.clone(),
        ..Default::default()
    };
    let signed = {
        let session = state.session.lock().await;
        session.sign_invoke_for(account, calls, chain, &params_v3)?
    };
    let sender = Felt::from_hex(&account.address)
        .map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;

    if submit {
        let node = state.node_for(chain).ok_or(WalletRpcError::NoNode)?;
        let hash = node
            .add_invoke(
                &sender,
                &signed.calldata,
                &[signed.r, signed.s],
                &nonce,
                &bounds,
                &proof_facts,
                proof.as_deref(),
            )
            .await
            .map_err(|e| WalletRpcError::Node(e.to_string()))?;
        return Ok(json!({ "transaction_hash": felt_hex(&hash), "submitted": true }));
    }

    // Sign-only: return a COMPLETE, canonical-hex INVOKE_TXN_V3 (signature + all
    // fields incl. proof) ready to broadcast as-is — no hand-assembly needed.
    let tx = crate::node::invoke_v3_tx_json(
        &sender,
        &signed.calldata,
        &[signed.r, signed.s],
        &nonce,
        &bounds,
        &proof_facts,
        proof.as_deref(),
    );
    Ok(json!({
        "transaction_hash": felt_hex(&signed.transaction_hash),
        "signature": [felt_hex(&signed.r), felt_hex(&signed.s)],
        "signed_transaction": tx,
        "submitted": false,
    }))
}

/// Human-readable one-line summary of a call for the approval prompt.
fn summarize_calls(calls: &[Call]) -> String {
    calls
        .iter()
        .map(|c| format!("→ {} :: selector {}", felt_hex(&c.to), felt_hex(&c.selector)))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Declare a contract class (DECLARE v3).
///
/// The `class_hash` a declare commits to is **derived by the node** from the
/// broadcast `contract_class`, and the account validates the signature against
/// the transaction hash built from *that* value — never from a caller-supplied
/// one. So when `contract_class` is given we derive the hash ourselves
/// (`SierraClass::class_hash`) and sign that; a caller-supplied `class_hash` is
/// cross-checked and a mismatch is a `114` naming both hashes, instead of an
/// opaque on-chain "invalid signature" (strkd #9). Without `contract_class`
/// (hash-only signing), `class_hash` is required and taken on trust.
///
/// Estimation and `submit:true` need `contract_class`. Sign-only returns a
/// complete `BROADCASTED_DECLARE_TXN_V3` (the class included when supplied).
async fn handle_add_declare(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let submit = params.get("submit").and_then(|v| v.as_bool()) == Some(true);
    let account_address = param_str(params, "account_address")?;
    let want = normalize_address(&account_address)?;

    let account = {
        let session = state.session.lock().await;
        let reg = session.registry()?;
        let found = reg
            .scoped_for(scope_for(client).as_deref())
            .find(|a| a.address == want)
            .cloned()
            .ok_or(WalletRpcError::Forbidden)?;
        found
    };
    let sender = Felt::from_hex(&account.address)
        .map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;

    // Accepts the RPC CONTRACT_CLASS object or scarb's *.contract_class.json
    // (ABI as an array, debug info) — normalized to the canonical RPC object so
    // what we hash is exactly what we broadcast.
    let contract_class = match params.get("contract_class") {
        Some(v) if !v.is_null() => Some(SierraClass::from_json(v)?),
        _ => None,
    };
    let given_class_hash = match params.get("class_hash") {
        Some(v) if !v.is_null() => Some(felt_from(v, "class_hash")?),
        _ => None,
    };
    let class_hash = match (&contract_class, given_class_hash) {
        (Some(cc), Some(given)) => {
            let derived = cc.class_hash();
            if derived != given {
                return Err(WalletRpcError::InvalidRequest(format!(
                    "class_hash {} does not match the supplied contract_class, which hashes to {}. \
                     The node derives the class hash from contract_class and validates the \
                     signature against THAT transaction hash, so signing {} would be rejected \
                     on-chain as an invalid signature. Check the class (the abi string is hashed \
                     byte-for-byte; pass the abi as the JSON array to have it serialised the way \
                     the compiler hashes it), or omit class_hash to sign the derived one.",
                    felt_hex(&given),
                    felt_hex(&derived),
                    felt_hex(&given),
                )));
            }
            derived
        }
        (Some(cc), None) => cc.class_hash(),
        (None, Some(given)) => given,
        (None, None) => {
            return Err(WalletRpcError::InvalidRequest(
                "missing 'class_hash' (or pass 'contract_class' and it is derived)".into(),
            ))
        }
    };
    let compiled_class_hash = felt_from(
        params
            .get("compiled_class_hash")
            .ok_or_else(|| WalletRpcError::InvalidRequest("missing 'compiled_class_hash'".into()))?,
        "compiled_class_hash",
    )?;
    let contract_class_json = contract_class.as_ref().map(SierraClass::to_rpc_json);
    let chain = resolve_chain(state, params).await?;

    // Nonce: caller-supplied or node-fetched.
    let nonce = match opt_nonce(params)? {
        Some(n) => n,
        None => {
            let node = state.node_for(chain).ok_or(WalletRpcError::NoNode)?;
            node.get_nonce(&sender)
                .await
                .map_err(|e| WalletRpcError::Node(e.to_string()))?
        }
    };
    // Fee bounds: caller-supplied, or estimate (needs node + contract_class).
    let bounds = match opt_fee_bounds(params)? {
        Some(b) => b,
        None => {
            let node = state.node_for(chain).ok_or(WalletRpcError::NoNode)?;
            let cc = contract_class_json.as_ref().ok_or_else(|| {
                WalletRpcError::InvalidRequest(
                    "to estimate a declare, provide 'contract_class' (or pass resource_bounds)".into(),
                )
            })?;
            node.estimate_declare(&sender, &compiled_class_hash, cc, &nonce)
                .await
                .map_err(|e| WalletRpcError::Node(e.to_string()))?
        }
    };

    let decision = gated_approval(
        state,
        client,
        "wallet_addDeclareTransaction",
        format!(
            "{} class {} from {} on {}. Max fee: {}",
            if submit { "Declare" } else { "Sign declaration of" },
            felt_hex(&class_hash),
            account.address,
            chain_name(chain),
            bounds_summary(&bounds)
        ),
    )
    .await;
    if decision == Decision::Reject {
        return Err(WalletRpcError::UserRefused);
    }

    let params_v3 = InvokeV3Params {
        nonce,
        tip: 0,
        l1_gas: bounds.l1_gas,
        l2_gas: bounds.l2_gas,
        l1_data_gas: bounds.l1_data_gas,
        ..Default::default()
    };
    let signed = {
        let session = state.session.lock().await;
        session.sign_declare_for(&account, &class_hash, &compiled_class_hash, chain, &params_v3)?
    };

    if submit {
        let node = state.node_for(chain).ok_or(WalletRpcError::NoNode)?;
        let cc = contract_class_json.as_ref().ok_or_else(|| {
            WalletRpcError::InvalidRequest("submit:true requires 'contract_class'".into())
        })?;
        let hash = node
            .add_declare(&sender, &compiled_class_hash, cc, &[signed.r, signed.s], &nonce, &bounds)
            .await
            .map_err(|e| WalletRpcError::Node(e.to_string()))?;
        return Ok(json!({
            "transaction_hash": felt_hex(&hash),
            "class_hash": felt_hex(&class_hash),
            "submitted": true,
        }));
    }

    // Sign-only: a COMPLETE, canonical-hex BROADCASTED_DECLARE_TXN_V3 ready to
    // POST as `declare_transaction`. The class rides along when supplied; if
    // not, the caller splices in the exact class that hashes to `class_hash`.
    let tx = crate::node::declare_v3_tx_json(
        &sender,
        &compiled_class_hash,
        contract_class_json.as_ref(),
        &[signed.r, signed.s],
        &nonce,
        &bounds,
    );
    Ok(json!({
        "transaction_hash": felt_hex(&signed.transaction_hash),
        "class_hash": felt_hex(&class_hash),
        "signature": [felt_hex(&signed.r), felt_hex(&signed.s)],
        "signed_transaction": tx,
        "submitted": false,
    }))
}

/// Normalize and sanity-check a base64-encoded SNIP-36 proof before it rides
/// along on a broadcast. Proof artifacts are commonly newline-terminated (the
/// prover CLI writes a trailing newline) or line-wrapped, and a stray newline or
/// a url-safe-alphabet proof is a likely cause of the node rejecting the `proof`
/// field. Strip ASCII whitespace and reject anything outside the standard base64
/// alphabet here, with a clear error, rather than letting it surface as an opaque
/// node error on broadcast. Intentionally no length/`%4` check — unpadded base64
/// is legal and a strict check would falsely reject it.
fn normalize_proof_b64(raw: &str) -> Result<String, WalletRpcError> {
    let cleaned: String = raw.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let bytes = cleaned.as_bytes();
    let pad = bytes.iter().rev().take_while(|&&b| b == b'=').count();
    let body = &bytes[..bytes.len() - pad];
    if body.is_empty() {
        return Err(WalletRpcError::InvalidRequest(
            "proof is empty or all padding after trimming whitespace".into(),
        ));
    }
    if pad > 2 || body.iter().any(|&b| !(b.is_ascii_alphanumeric() || b == b'+' || b == b'/')) {
        return Err(WalletRpcError::InvalidRequest(
            "proof is not valid standard base64 (expected A-Za-z0-9+/ with optional trailing '='; \
             check for url-safe '-'/'_', embedded '=' padding, or stray characters)"
                .into(),
        ));
    }
    Ok(cleaned)
}

async fn handle_add_invoke(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let submit = params.get("submit").and_then(|v| v.as_bool()) == Some(true);
    let account_address = param_str(params, "account_address")?;
    let want = normalize_address(&account_address)?;
    let calls = parse_calls(params)?;

    // Optional SNIP-36 proof-carrying invoke: proof_facts extends the signed
    // hash; proof rides along on broadcast.
    let proof_facts: Vec<Felt> = match params.get("proof_facts") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| felt_from(v, "proof_facts element"))
            .collect::<Result<Vec<_>, _>>()?,
        None | Some(Value::Null) => Vec::new(),
        _ => return Err(WalletRpcError::InvalidRequest("proof_facts must be an array".into())),
    };
    let proof: Option<String> = match params.get("proof").and_then(|v| v.as_str()) {
        Some(s) => Some(normalize_proof_b64(s)?),
        None => None,
    };
    if submit && !proof_facts.is_empty() && proof.is_none() {
        return Err(WalletRpcError::InvalidRequest(
            "broadcasting a proof-carrying invoke (proof_facts) requires 'proof'".into(),
        ));
    }

    // Resolve the signing account within the caller's scope.
    let account = {
        let session = state.session.lock().await;
        let reg = session.registry()?;
        let found = reg
            .scoped_for(scope_for(client).as_deref())
            .find(|a| a.address == want)
            .cloned()
            .ok_or(WalletRpcError::Forbidden)?;
        found
    };
    let sender = Felt::from_hex(&account.address)
        .map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;
    let chain = resolve_chain(state, params).await?;

    // Nonce + fee: caller-supplied or node-resolved (before the prompt, so the
    // fee can be shown).
    let encoded = wallet_core::encode_calls(&calls);
    let (nonce, bounds) =
        resolve_exec(state, chain, &sender, &encoded, params, !proof_facts.is_empty()).await?;

    let kind = if proof_facts.is_empty() { "" } else { " (SNIP-36 proof-carrying)" };
    let decision = gated_approval(
        state,
        client,
        "wallet_addInvokeTransaction",
        format!(
            "{} {} call(s){} from {} on {}. {}. Max fee: {}",
            if submit { "Submit" } else { "Sign" },
            calls.len(),
            kind,
            account.address,
            chain_name(chain),
            summarize_calls(&calls),
            bounds_summary(&bounds)
        ),
    )
    .await;
    if decision == Decision::Reject {
        return Err(WalletRpcError::UserRefused);
    }

    sign_and_submit(state, chain, &account, &calls, nonce, bounds, submit, proof_facts, proof).await
}

/// Turn an agent's structured feedback into a **prefilled GitHub "new issue"
/// link**. strkd is alpha; when an agent hits a limitation it should report it
/// (see the `alpha_notice` in the usage doc) rather than work around it. This
/// files NOTHING and stores no credential — it returns a URL the operator opens,
/// reviews, edits, and submits in their own browser under their own GitHub
/// identity (so repo access / human review are the gate). Synchronous: it builds
/// a string, touches no session/key material, and works whether locked or not.
fn handle_report_issue(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    // goal + needed are the two fields a fix actually needs; the rest mirror the
    // STRKD-FEEDBACK template and are optional.
    let goal = param_str(params, "goal")?;
    let needed = param_str(params, "needed")?;
    if goal.trim().is_empty() || needed.trim().is_empty() {
        return Err(WalletRpcError::InvalidRequest(
            "`goal` and `needed` must be non-empty".into(),
        ));
    }

    let raw_title = opt_param_str(params, "title").unwrap_or_else(|| goal.clone());
    // Tagged so the maintainer can filter agent-filed issues.
    let title = format!("[agent-feedback] {}", truncate_chars(raw_title.trim(), 120));

    let mut body = String::new();
    push_field(&mut body, "Goal", &goal);
    push_field(&mut body, "Needed", &needed);
    for (label, key) in [
        ("Attempted", "attempted"),
        ("Observed", "observed"),
        ("Limitation", "limitation"),
        ("Impact", "impact"),
        ("Workaround avoided", "workaround_avoided"),
        ("Context", "context"),
    ] {
        if let Some(v) = opt_param_str(params, key) {
            push_field(&mut body, label, &v);
        }
    }
    body.push_str(&format!(
        "---\n_Filed via strkd `companion_reportIssue` by **{}** ({}). strkd is alpha — \
review and edit before submitting, and remove anything sensitive._",
        client.label, client.id
    ));
    // Keep the prefilled URL within practical browser/server limits (~8 KB once
    // encoded); the operator can add detail in the browser if it's clipped.
    let body = truncate_chars(&body, 5000);

    let url = format!(
        "https://github.com/{}/issues/new?title={}&body={}",
        state.issue_repo,
        encode_query(&title),
        encode_query(&body),
    );

    Ok(json!({
        "url": url,
        "title": title,
        "body": body,
        "repo": state.issue_repo,
        // Make it unambiguous that strkd posted nothing.
        "filed": false,
        "instructions": "Give this `url` to your operator. Opening it loads a prefilled GitHub \
issue draft; they review, edit, and submit it in their browser. strkd does not file it for you, \
and stores no GitHub credential."
    }))
}

// ── On-device proving (companion_prove*) ─────────────────────────────────────
//
// Proving is folded onto this authenticated loopback service rather than a
// separate open HTTP port: every call goes through pairing, the transport
// guard, and the request log like any other method. The payload stays generic
// (opaque) — the prover never signs and holds no key material; it proves an
// **already-signed** transaction and returns the proof.

/// The wired prover, or a clean "not available" error when none is attached
/// (e.g. a headless service started without `with_prover`).
fn prover_or_err(state: &ServerState) -> Result<&prover::ProverState, WalletRpcError> {
    state.prover.as_deref().ok_or_else(|| {
        WalletRpcError::NotImplemented("on-device proving is not available on this service".into())
    })
}

/// `companion_prove` — enqueue an opaque, already-signed payload for on-device
/// proving. Returns immediately with a job id; poll `companion_proveStatus`.
async fn handle_prove(state: &ServerState, params: &Value) -> Result<Value, WalletRpcError> {
    let prover = prover_or_err(state)?;
    let network = opt_param_str(params, "network").unwrap_or_else(|| "mainnet".into());
    let label = opt_param_str(params, "label");
    // `payload` is the opaque thing to prove; if absent, treat the whole params
    // object as the payload (mirrors the prover crate's generic contract).
    let payload = params.get("payload").cloned().unwrap_or_else(|| params.clone());
    let job_id = prover::enqueue_prove(prover, payload, label, network).await;
    Ok(json!({
        "job_id": job_id,
        "status": "queued",
        "next": "poll companion_proveStatus { job_id } until status is \"succeeded\" or \"failed\"",
    }))
}

/// `companion_proveStatus` — poll a proving job by id (status + result/error).
async fn handle_prove_status(state: &ServerState, params: &Value) -> Result<Value, WalletRpcError> {
    let prover = prover_or_err(state)?;
    let job_id = param_str(params, "job_id")?;
    match prover.jobs.get(&job_id).await {
        Some(job) => Ok(json!(job)),
        None => Err(WalletRpcError::InvalidRequest(format!("unknown job '{job_id}'"))),
    }
}

/// `companion_proofActivity` — recent proving activity feed (most-recent first).
async fn handle_proof_activity(state: &ServerState) -> Result<Value, WalletRpcError> {
    let prover = prover_or_err(state)?;
    Ok(json!({ "activity": prover.jobs.recent_activity().await }))
}

/// The prover's network name for a chain (`mainnet`/`testnet`) — selects which
/// per-network prover settings (RPC + remote prover) the prove uses.
fn prover_network(chain: ChainId) -> &'static str {
    match chain {
        ChainId::Mainnet => "mainnet",
        ChainId::Sepolia => "testnet",
    }
}

/// `companion_signAndProve` — sign a **virtual** invoke (SNIP-36 "Tx A") and prove
/// it on-device in one step, so the wallet's signing is wired straight to the
/// in-process prover (the secret never leaves the machine and the caller skips
/// the manual sign-only → companion_prove round-trip).
///
/// This signs Tx A — a normal `INVOKE_TXN_V3` calling the contract's virtual
/// function — and hands it to the prover. Tx A is **not** proof-carrying:
/// `proof_facts` are an *output* of proving, not an input here, so the signed
/// hash is a standard v3 hash. The proof comes back via `companion_proveStatus`
/// as `{ proof, proof_facts, l2_to_l1_messages }`.
///
/// What it does NOT do: build or broadcast the on-chain verifier invoke ("Tx B",
/// e.g. `verify_result(public_message)`). Tx B's calldata is decoded from the
/// prover's L2→L1 output and is application-specific, so the caller assembles it
/// and broadcasts via `wallet_addInvokeTransaction { proof_facts, proof,
/// submit:true }`. (A generic wallet can't know the verifier's call shape.)
async fn handle_sign_and_prove(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let prover = prover_or_err(state)?;
    let account_address = param_str(params, "account_address")?;
    let want = normalize_address(&account_address)?;
    let calls = parse_calls(params)?;
    let chain = resolve_chain(state, params).await?;

    // Resolve the signing account within the caller's scope (same as add_invoke).
    let account = {
        let session = state.session.lock().await;
        let reg = session.registry()?;
        let found = reg
            .scoped_for(scope_for(client).as_deref())
            .find(|a| a.address == want)
            .cloned()
            .ok_or(WalletRpcError::Forbidden)?;
        found
    };
    let sender = Felt::from_hex(&account.address)
        .map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;

    // Nonce: caller-supplied, else the account nonce from the node. The virtual
    // tx nonce must equal the account nonce AT THE REFERENCE BLOCK — the prover's
    // preflight validates that and errors clearly on mismatch.
    let nonce = match opt_nonce(params)? {
        Some(n) => n,
        None => {
            let node = state.node_for(chain).ok_or(WalletRpcError::NoNode)?;
            node.get_nonce(&sender)
                .await
                .map_err(|e| WalletRpcError::Node(e.to_string()))?
        }
    };

    // resource_bounds MUST be explicit. Estimating a virtual tx online would send
    // its (private) calldata to the RPC node, defeating SNIP-36's privacy — so we
    // refuse to auto-estimate here regardless of whether a node is configured.
    let bounds = opt_fee_bounds(params)?.ok_or_else(|| {
        WalletRpcError::InvalidRequest(
            "companion_signAndProve requires explicit resource_bounds: the virtual tx carries \
             private calldata, so it must NOT be fee-estimated online (that would send the \
             private inputs to the RPC node). Set resource_bounds manually (~2× current gas \
             prices)."
                .into(),
        )
    })?;

    // Signing a real tx on the user's account → gate it (proven locally, never
    // broadcast by strkd).
    let decision = gated_approval(
        state,
        client,
        "companion_signAndProve",
        format!(
            "Sign + locally prove {} virtual call(s) from {} on {} (SNIP-36; the signed tx is \
             proven on-device, never broadcast). {}. Max fee: {}",
            calls.len(),
            account.address,
            chain_name(chain),
            summarize_calls(&calls),
            bounds_summary(&bounds),
        ),
    )
    .await;
    if decision == Decision::Reject {
        return Err(WalletRpcError::UserRefused);
    }

    // Sign Tx A — standard signed INVOKE_TXN_V3 (proof_facts empty: not proof-carrying).
    let signed = {
        let session = state.session.lock().await;
        session.sign_invoke_for(
            &account,
            &calls,
            chain,
            &InvokeV3Params {
                nonce,
                tip: 0,
                l1_gas: bounds.l1_gas,
                l2_gas: bounds.l2_gas,
                l1_data_gas: bounds.l1_data_gas,
                ..Default::default()
            },
        )?
    };

    // Serialize the complete INVOKE_TXN_V3 (no proof) — the input the prover proves.
    let tx_json = crate::node::invoke_v3_tx_json(
        &sender,
        &signed.calldata,
        &[signed.r, signed.s],
        &nonce,
        &bounds,
        &[],
        None,
    );

    // Hand the signed virtual tx to the in-process prover. block_number is the
    // reference block (optional; the prover defaults to a recent block).
    let mut payload = json!({ "transaction": tx_json });
    if let Some(bn) = params.get("block_number").and_then(|v| v.as_u64()) {
        payload["block_number"] = json!(bn);
    }
    let label = opt_param_str(params, "label");
    let job_id =
        prover::enqueue_prove(prover, payload, label, prover_network(chain).to_string()).await;

    Ok(json!({
        "job_id": job_id,
        "status": "queued",
        "transaction_hash": felt_hex(&signed.transaction_hash),
        "next": "poll companion_proveStatus { job_id }; on success take result.proof / \
                 result.proof_facts / result.l2_to_l1_messages, build the verifier invoke from \
                 the message, and broadcast it via wallet_addInvokeTransaction \
                 { proof_facts, proof, submit:true }",
    }))
}

/// Reveal the funding-source (manager) account address so an agent can look up
/// its nonce before requesting funding. No approval — it's just an address the
/// agent will see in any funding tx anyway.
async fn handle_funding_source(state: &ServerState) -> Result<Value, WalletRpcError> {
    let session = state.session.lock().await;
    let manager = session.manager_account(0)?;
    Ok(json!({ "address": manager.address, "index": 0 }))
}

fn bounds_json(b: &FeeBounds) -> Value {
    let rb = |r: &wallet_core::ResourceBounds| {
        json!({
            "max_amount": format!("0x{:x}", r.max_amount),
            "max_price_per_unit": format!("0x{:x}", r.max_price_per_unit),
        })
    };
    json!({ "l1_gas": rb(&b.l1_gas), "l2_gas": rb(&b.l2_gas), "l1_data_gas": rb(&b.l1_data_gas) })
}

/// Agent-initiated request for an auto-approval grant. **Always prompts** (a
/// permission escalation is never auto-approved, even under an existing grant).
/// On approval, grants this client a window of `days` (clamped 1..=90).
async fn handle_request_grant(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let days = params
        .get("days")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .clamp(1, 90);

    let decision = state
        .approver
        .request_approval(ApprovalRequest {
            client_label: format!("{} ({})", client.label, client.id),
            method: "companion_requestGrant".into(),
            summary: format!(
                "Agent {} ({}) requests auto-approval for {days} day(s) — its own-account \
operations would skip prompts (funding still always prompts). Approve?",
                client.label, client.id
            ),
        })
        .await;
    if decision == Decision::Reject {
        return Err(WalletRpcError::UserRefused);
    }

    let until = now_unix_ms() + days * 24 * 60 * 60 * 1000;
    state.clients.lock().await.grant(&client.id, until);
    Ok(json!({ "granted": true, "expires_at": until, "days": days }))
}

/// Opt-in fee estimate for a sign-only caller: returns suggested `resource_bounds`
/// (canonical hex, margin applied) and the nonce from the configured node, so a
/// caller that signs here but broadcasts elsewhere doesn't hand-compute bounds.
/// No prompt, no signing. Needs a node (else -32005).
///
/// Opt-in by design: do NOT call this for SNIP-36 virtual txs with private
/// calldata — it sends the calls to the node for simulation.
async fn handle_estimate_fee(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let account_address = param_str(params, "account_address")?;
    let want = normalize_address(&account_address)?;
    let calls = parse_calls(params)?;
    let account = {
        let session = state.session.lock().await;
        let reg = session.registry()?;
        let found = reg
            .scoped_for(scope_for(client).as_deref())
            .find(|a| a.address == want)
            .cloned()
            .ok_or(WalletRpcError::Forbidden)?;
        found
    };
    let sender = Felt::from_hex(&account.address)
        .map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;
    let chain = resolve_chain(state, params).await?;
    let node = state.node_for(chain).ok_or(WalletRpcError::NoNode)?;

    let nonce = match opt_nonce(params)? {
        Some(n) => n,
        None => node
            .get_nonce(&sender)
            .await
            .map_err(|e| WalletRpcError::Node(e.to_string()))?,
    };
    let encoded = wallet_core::encode_calls(&calls);
    let bounds = node
        .estimate_invoke(&sender, &encoded, &nonce)
        .await
        .map_err(|e| WalletRpcError::Node(e.to_string()))?;

    Ok(json!({
        "nonce": felt_hex(&nonce),
        "resource_bounds": bounds_json(&bounds),
    }))
}

/// Switch the active *default* network (Sepolia ⇄ Mainnet). `chainId` is the
/// felt-encoded CHAIN_ID. Switching also switches which per-network node is used.
///
/// DEPRECATED for agents: this mutates a single shared default across ALL paired
/// clients, so one client can change the network out from under another. Prefer
/// passing a per-request `chainId` to the operational methods (invoke / declare /
/// deploy / estimateFee / requestFunding) instead — it's explicit and race-free.
/// Kept for EIP-1193 / browser-wallet compatibility and to set the fallback used
/// when a request omits `chainId`. (The human sets the default in the desktop
/// Settings; agents rarely need this.)
async fn handle_switch_chain(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let chain_id_str = param_str(params, "chainId")?;
    let felt = Felt::from_hex(&chain_id_str)
        .map_err(|_| WalletRpcError::InvalidRequest("invalid chainId".into()))?;
    let target = ChainId::from_felt(&felt).map_err(|_| WalletRpcError::ChainIdNotSupported)?;

    let decision = state
        .approver
        .request_approval(ApprovalRequest {
            client_label: format!("{} ({})", client.label, client.id),
            method: "wallet_switchStarknetChain".into(),
            summary: format!("Switch the wallet network to {}", chain_name(target)),
        })
        .await;
    if decision == Decision::Reject {
        return Err(WalletRpcError::UserRefused);
    }

    state.session.lock().await.set_chain(target);
    Ok(json!(true))
}

/// Add a token to the wallet's watch list (display-only this phase).
async fn handle_watch_asset(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    // Accept either a top-level `asset`/`options` object or the params themselves.
    let asset = params
        .get("asset")
        .or_else(|| params.get("options"))
        .cloned()
        .unwrap_or_else(|| params.clone());
    let address = asset
        .get("address")
        .and_then(|v| v.as_str())
        .ok_or_else(|| WalletRpcError::InvalidRequest("asset missing 'address'".into()))?
        .to_string();
    let symbol = asset
        .get("symbol")
        .and_then(|v| v.as_str())
        .unwrap_or("token")
        .to_string();

    let decision = state
        .approver
        .request_approval(ApprovalRequest {
            client_label: format!("{} ({})", client.label, client.id),
            method: "wallet_watchAsset".into(),
            summary: format!("Watch token {symbol} at {address}"),
        })
        .await;
    if decision == Decision::Reject {
        return Err(WalletRpcError::UserRefused);
    }

    state.add_watched_asset(asset);
    Ok(json!(true))
}

/// Agent-initiated funding request: build a STRK transfer **from the manager
/// account to one of the agent's own accounts**, prompt the user, and (on
/// approval) sign it — and broadcast it when `submit:true` + a node is
/// configured. The manager pays the fee. The agent never chooses the sender; it
/// can only pull funds into its own scoped accounts.
///
/// With a node configured, nonce + fee are resolved automatically (turnkey);
/// without one, the caller supplies `nonce` + `resource_bounds` (sign-only).
async fn handle_request_funding(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    if client.kind != ClientKind::Agent {
        return Err(WalletRpcError::Forbidden);
    }
    let submit = params.get("submit").and_then(|v| v.as_bool()) == Some(true);

    // Amount in fri (STRK's smallest unit); fits in u128 for any real amount.
    let amount = parse_u128_field(params, "amount")?;
    if amount == 0 {
        return Err(WalletRpcError::InvalidRequest("amount must be > 0".into()));
    }
    let token_str =
        opt_param_str(params, "token").unwrap_or_else(|| STRK_TOKEN_ADDRESS.to_string());
    let token = Felt::from_hex(&token_str)
        .map_err(|_| WalletRpcError::InvalidRequest("bad token address".into()))?;
    let manager_index = params
        .get("funding_source_index")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    // Resolve the recipient: an explicit address (must be one of the caller's
    // own accounts) or the caller's first account. Enforces that funds can only
    // flow into the agent's scope.
    let recipient_acct = {
        let session = state.session.lock().await;
        let reg = session.registry()?;
        let scope = scope_for(client);
        let want = match opt_param_str(params, "recipient") {
            Some(s) => Some(normalize_address(&s)?),
            None => None,
        };
        let found = match want {
            Some(addr) => reg
                .scoped_for(scope.as_deref())
                .find(|a| a.address == addr)
                .cloned(),
            None => reg.scoped_for(scope.as_deref()).next().cloned(),
        };
        found.ok_or_else(|| {
            WalletRpcError::InvalidRequest(
                "no account to fund (create one with companion_createAgentAccount, or pass a recipient you own)".into(),
            )
        })?
    };
    let recipient = Felt::from_hex(&recipient_acct.address)
        .map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;

    // Resolve the manager (sender) — the agent does NOT choose this.
    let manager = {
        let session = state.session.lock().await;
        session.manager_account(manager_index)?
    };
    let manager_sender = Felt::from_hex(&manager.address)
        .map_err(|_| WalletRpcError::Unknown("bad manager address".into()))?;

    // STRK transfer(recipient, amount_low, amount_high=0).
    let call = Call {
        to: token,
        selector: wallet_core::get_selector_from_name("transfer"),
        calldata: vec![recipient, Felt::from(amount), Felt::ZERO],
    };
    let calls = vec![call];

    // Nonce + fee for the manager: caller-supplied or node-resolved.
    let chain = resolve_chain(state, params).await?;

    // Pre-check: the manager pays the transfer fee, so it must be deployed on the
    // active network. If it isn't, fee-estimation below would fail with an opaque
    // node "Contract not found" — surface a clear, actionable error instead.
    if let Some(node) = state.node_for(chain) {
        if !node
            .is_deployed(&manager_sender)
            .await
            .map_err(|e| WalletRpcError::Node(e.to_string()))?
        {
            return Err(WalletRpcError::Precondition(format!(
                "funding source (manager) account {} is not deployed on {}. \
Deploy and fund it on {} first — it pays the transfer fee.",
                manager.address,
                chain_name(chain),
                chain_name(chain),
            )));
        }
    }

    let encoded = wallet_core::encode_calls(&calls);
    // Manager/funding invokes are never proof-carrying.
    let (nonce, bounds) =
        resolve_exec(state, chain, &manager_sender, &encoded, params, false).await?;

    let strk = amount as f64 / 1e18;
    let decision = state
        .approver
        .request_approval(ApprovalRequest {
            client_label: format!("{} ({})", client.label, client.id),
            method: "companion_requestFunding".into(),
            summary: format!(
                "Agent {} ({}) requests a top-up of {strk:.4} STRK ({amount} fri) on {} → account {}",
                client.label,
                client.id,
                chain_name(chain),
                recipient_acct.address
            ),
        })
        .await;
    if decision == Decision::Reject {
        return Err(WalletRpcError::UserRefused);
    }

    let mut result =
        sign_and_submit(state, chain, &manager, &calls, nonce, bounds, submit, Vec::new(), None)
            .await?;
    // Annotate with funding-specific fields.
    if let Some(obj) = result.as_object_mut() {
        obj.insert("recipient".into(), json!(recipient_acct.address));
        obj.insert("amount".into(), json!(amount.to_string()));
        obj.insert("token".into(), json!(token_str));
    }
    Ok(result)
}

/// Deploy one of the caller's own accounts (a DEPLOY_ACCOUNT v3). Sign-only by
/// default (returns the signed deploy tx — broadcast it yourself, keyless);
/// `submit:true` broadcasts via the node. The account must already hold funds to
/// pay its own deploy fee. With a node, the fee is auto-estimated; otherwise pass
/// `resource_bounds`. The account is chosen within the caller's scope.
async fn handle_deploy_account(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let submit = params.get("submit").and_then(|v| v.as_bool()) == Some(true);

    // Resolve the account to deploy: explicit `account` (must be in scope) or the
    // caller's first scoped account.
    let account = {
        let session = state.session.lock().await;
        let reg = session.registry()?;
        let scope = scope_for(client);
        let want = match opt_param_str(params, "account") {
            Some(s) => Some(normalize_address(&s)?),
            None => None,
        };
        let found = match want {
            Some(addr) => reg
                .scoped_for(scope.as_deref())
                .find(|a| a.address == addr)
                .cloned(),
            None => reg.scoped_for(scope.as_deref()).next().cloned(),
        };
        found.ok_or_else(|| {
            WalletRpcError::InvalidRequest("no account in scope to deploy".into())
        })?
    };

    let chain = resolve_chain(state, params).await?;

    // Pre-check: refuse to (re)deploy an already-deployed account. Without this a
    // second deploy reuses nonce 0 and the node rejects it with a confusing
    // "invalid nonce" error (the account's nonce is already 1).
    if let Some(node) = state.node_for(chain) {
        let addr = Felt::from_hex(&account.address)
            .map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;
        if node
            .is_deployed(&addr)
            .await
            .map_err(|e| WalletRpcError::Node(e.to_string()))?
        {
            return Err(WalletRpcError::Precondition(format!(
                "account {} is already deployed on {}",
                account.address,
                chain_name(chain),
            )));
        }
    }

    // Deployment data (class hash, salt, ctor calldata) for the fee estimate.
    let d = {
        let session = state.session.lock().await;
        session.deployment_data_for(&account)?
    };

    // Fee: caller-supplied or node-estimated (deploy_account nonce is always 0).
    let bounds = match opt_fee_bounds(params)? {
        Some(b) => b,
        None => {
            let node = state.node_for(chain).ok_or(WalletRpcError::NoNode)?;
            node.estimate_deploy_account(&d.address, &d.class_hash, &d.constructor_calldata, &d.salt)
                .await
                .map_err(|e| WalletRpcError::Node(e.to_string()))?
        }
    };

    let decision = gated_approval(
        state,
        client,
        "companion_deployAccount",
        format!(
            "{} account {} on {}. Max fee: {}",
            if submit { "Deploy" } else { "Sign deployment of" },
            account.address,
            chain_name(chain),
            bounds_summary(&bounds)
        ),
    )
    .await;
    if decision == Decision::Reject {
        return Err(WalletRpcError::UserRefused);
    }

    let params_v3 = InvokeV3Params {
        nonce: Felt::ZERO,
        tip: 0,
        l1_gas: bounds.l1_gas,
        l2_gas: bounds.l2_gas,
        l1_data_gas: bounds.l1_data_gas,
        ..Default::default()
    };
    let signed = {
        let session = state.session.lock().await;
        session.sign_deploy_account_for(&account, chain, &params_v3)?
    };

    if submit {
        let node = state.node_for(chain).ok_or(WalletRpcError::NoNode)?;
        let hash = node
            .add_deploy_account(
                &signed.class_hash,
                &signed.constructor_calldata,
                &signed.salt,
                &[signed.r, signed.s],
                &bounds,
            )
            .await
            .map_err(|e| WalletRpcError::Node(e.to_string()))?;
        return Ok(json!({
            "transaction_hash": felt_hex(&hash),
            "contract_address": account.address,
            "submitted": true,
        }));
    }

    Ok(json!({
        "transaction_hash": felt_hex(&signed.transaction_hash),
        "contract_address": account.address,
        "signature": [felt_hex(&signed.r), felt_hex(&signed.s)],
        "signed_transaction": {
            "type": "DEPLOY_ACCOUNT",
            "version": "0x3",
            "class_hash": felt_hex(&signed.class_hash),
            "contract_address_salt": felt_hex(&signed.salt),
            "constructor_calldata": signed.constructor_calldata.iter().map(felt_hex).collect::<Vec<_>>(),
            "nonce": "0x0",
        },
        "submitted": false,
    }))
}

async fn handle_create_agent_account(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    if client.kind != ClientKind::Agent {
        return Err(WalletRpcError::Forbidden);
    }
    let label = opt_param_str(params, "label").unwrap_or_else(|| client.label.clone());

    let decision = gated_approval(
        state,
        client,
        "companion_createAgentAccount",
        format!("Create a new agent account labelled \"{label}\""),
    )
    .await;
    if decision == Decision::Reject {
        return Err(WalletRpcError::UserRefused);
    }

    let mut session = state.session.lock().await;
    let account = session.create_agent_account(&client.id, label)?;

    // Persist transactionally: if a vault store is configured, re-seal and write
    // it back atomically. On any failure, roll back the in-memory add so memory
    // and disk stay consistent. (If no store is configured the change is
    // in-memory only — e.g. tests.)
    if let Some(store) = &state.vault_store {
        let persisted = session
            .reseal()
            .and_then(|vault| {
                store
                    .save(&vault)
                    .map_err(|_| WalletRpcError::Unknown("failed to persist vault".into()))
            });
        if let Err(e) = persisted {
            session.rollback_account(&account.address);
            return Err(e);
        }
    }

    Ok(account_to_json(&account))
}

pub(crate) fn normalize_address(s: &str) -> Result<String, WalletRpcError> {
    let f = Felt::from_hex(s)
        .map_err(|_| WalletRpcError::InvalidRequest("invalid account_address".into()))?;
    Ok(address_hex(&f))
}
