//! strkd desktop (Tauri 2). Menu-bar companion that hosts the wallet core:
//! starts the loopback JSON-RPC service (so agents/apps can call it), surfaces
//! approval prompts from the service as menu-bar confirmation dialogs, and
//! exposes IPC commands for the app's own UI (onboarding, unlock, accounts,
//! request log).
//!
//! No key material crosses into this crate's own logic beyond what `wallet-rpc`
//! / `wallet-core` already own — the session lives inside the shared
//! `ServerState`. The one exception is onboarding, where a freshly generated
//! mnemonic is briefly held (zeroized) so the user can back it up and set a
//! passphrase.

mod settings;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use settings::Settings;

use tauri::image::Image;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

const TRAY_ID: &str = "strkd-tray";
/// The tray icon is derived from the same generated icon as the app/Dock icon
/// (`tauri icon` regenerates `128x128.png`), so changing `app-icon.png` updates
/// the menu-bar icon too. The pending "red dot" variant is overlaid at runtime.
const TRAY_BASE: &[u8] = include_bytes!("../icons/128x128.png");

fn tray_image(dot: bool) -> Option<Image<'static>> {
    if !dot {
        return Image::from_bytes(TRAY_BASE).ok();
    }
    // Overlay a red notification dot (top-right) on the base icon.
    let mut rgba = image::load_from_memory(TRAY_BASE).ok()?.to_rgba8();
    let (w, h) = rgba.dimensions();
    let (cx, cy, r) = (w as f32 * 0.74, h as f32 * 0.26, w as f32 * 0.20);
    let ring = w as f32 * 0.04;
    for y in 0..h {
        for x in 0..w {
            let d = (((x as f32 - cx).powi(2)) + ((y as f32 - cy).powi(2))).sqrt();
            if d <= r {
                rgba.put_pixel(x, y, image::Rgba([255, 59, 48, 255])); // red
            } else if d <= r + ring {
                rgba.put_pixel(x, y, image::Rgba([245, 245, 250, 255])); // light border
            }
        }
    }
    Some(Image::new_owned(rgba.into_raw(), w, h))
}

/// Reflect the pending-approval count on every surface we have: a red-dot tray
/// icon + tooltip in the menu bar, AND a Dock-tile badge (the standard macOS
/// count bubble). Both clear when nothing is pending. The Dock badge is the most
/// reliable signal — it shows even if OS notification permission is denied, and
/// needs the `Regular` activation policy (set in setup) so the app has a Dock
/// tile. Non-invasive (never steals window focus).
fn refresh_tray(app: &AppHandle, pending: usize) {
    // macOS requires menu-bar (NSStatusItem) and Dock-tile updates to run on the
    // main thread. refresh_tray is called from the approval bridge's async task,
    // so marshal onto the main thread — otherwise the red dot / badge can fail to
    // render even though the call "succeeds".
    let handle = app.clone();
    let res = app.run_on_main_thread(move || {
        if let Some(tray) = handle.tray_by_id(TRAY_ID) {
            let _ = tray.set_icon(tray_image(pending > 0));
            let tip = if pending > 0 {
                format!("strkd — {pending} pending request(s)")
            } else {
                "strkd — Starknet wallet companion".to_string()
            };
            let _ = tray.set_tooltip(Some(&tip));
        } else {
            eprintln!("[strkd] refresh_tray: tray '{TRAY_ID}' not found");
        }
        // Dock-tile badge: visible on the Dock icon regardless of notification perms.
        if let Some(w) = handle.get_webview_window("main") {
            let _ = w.set_badge_count(if pending > 0 { Some(pending as i64) } else { None });
        } else {
            eprintln!("[strkd] refresh_tray: main window not found");
        }
    });
    if let Err(e) = res {
        eprintln!("[strkd] refresh_tray: run_on_main_thread failed: {e}");
    }
}

use wallet_core::{generate_mnemonic, validate_mnemonic, AccountRef, ChainId, Registry};
use wallet_rpc::{
    bind_loopback, ChannelApprover, Decision, LogEntry, PendingApproval, RequestLog, ServerState,
    VaultStore, WalletSession,
};
use prover::{
    build_prover_state, Activity, ProofRecord, ProofSummary, ProverConfig, ProverState,
    Settings as ProverSettings, StorageStats,
};
use tauri_plugin_notification::NotificationExt;

/// Tauri-managed app state. The wallet session, clients, log and approver all
/// live inside the shared `ServerState` (which the loopback service also uses);
/// this struct adds the desktop-only bits.
struct DesktopState {
    server: Arc<ServerState>,
    vault_store: VaultStore,
    config_path: PathBuf,
    chain: ChainId,
    service_url: String,
    /// In-flight approval prompts awaiting a user decision, keyed by id.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Decision>>>>,
    /// Mnemonic staged during onboarding (between generate/import and finalize).
    onboarding: Mutex<Option<Zeroizing<String>>>,
    /// On-device proving companion (the same `Arc` is attached to `server` so
    /// the loopback `companion_prove*` methods and the Proving panel share one
    /// job/storage/settings store). Holds no key material.
    prover: Arc<ProverState>,
}

fn chain_name(c: ChainId) -> &'static str {
    match c {
        ChainId::Mainnet => "SN_MAIN",
        ChainId::Sepolia => "SN_SEPOLIA",
    }
}

/// Show + focus the main window (from the tray, or when a prompt arrives).
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Banner title for an incoming approval request (pairing and funding are the
/// ones a user most wants to catch).
fn notif_title(method: &str) -> &'static str {
    match method {
        "companion_requestPairing" => "strkd — pairing request",
        "companion_requestFunding" => "strkd — funding (top-up) request",
        _ => "strkd — approval needed",
    }
}

/// A macOS system sound name (guaranteed audible); a generic name elsewhere.
fn notif_sound() -> &'static str {
    if cfg!(target_os = "macos") {
        "Ping"
    } else {
        "default"
    }
}

// ---------------------------------------------------------------------------
// IPC commands
// ---------------------------------------------------------------------------

/// Overall app/service status for the UI's header + routing (onboarding vs
/// unlock vs main).
#[tauri::command]
async fn status(state: State<'_, DesktopState>) -> Result<serde_json::Value, String> {
    let session = state.server.session.lock().await;
    let accounts = session.registry().map(|r| r.accounts.len()).unwrap_or(0);
    Ok(serde_json::json!({
        "locked": session.is_locked(),
        "needs_onboarding": !state.vault_store.exists(),
        "network": chain_name(session.chain()),
        "version": env!("CARGO_PKG_VERSION"),
        "service_url": state.service_url,
        "accounts": accounts,
        // When false, the wallet is sign-only (no broadcast / fee estimation):
        // set STRKD_RPC_URL to enable. No URL echoed (it may embed an API key).
        "node_configured": state.server.has_node_for(session.chain()),
    }))
}

/// Generate a fresh mnemonic and stage it for backup. Returns it for one-time
/// display; the user confirms they've saved it before `finalize_setup`.
#[tauri::command]
async fn generate(state: State<'_, DesktopState>, word_count: usize) -> Result<String, String> {
    let m = generate_mnemonic(word_count).map_err(|e| e.to_string())?;
    *state.onboarding.lock().unwrap() = Some(Zeroizing::new(m.clone()));
    Ok(m)
}

/// Stage an imported mnemonic (validated) for `finalize_setup`.
#[tauri::command]
async fn import(state: State<'_, DesktopState>, phrase: String) -> Result<(), String> {
    validate_mnemonic(&phrase).map_err(|_| "invalid mnemonic".to_string())?;
    *state.onboarding.lock().unwrap() = Some(Zeroizing::new(phrase));
    Ok(())
}

/// Encrypt the staged mnemonic under `passphrase`, write the vault, and enter
/// the unlocked state.
#[tauri::command]
async fn finalize_setup(state: State<'_, DesktopState>, passphrase: String) -> Result<(), String> {
    let mnemonic = {
        let mut g = state.onboarding.lock().unwrap();
        g.take()
            .ok_or("no mnemonic staged; generate or import first")?
    };
    let session =
        WalletSession::new_unlocked(state.chain, mnemonic.to_string(), passphrase, Registry::default());
    let vault = session.reseal().map_err(|e| e.to_string())?;
    state.vault_store.save(&vault).map_err(|e| e.to_string())?;
    *state.server.session.lock().await = session;
    Ok(())
}

/// Unlock an existing vault with `passphrase`.
#[tauri::command]
async fn unlock(state: State<'_, DesktopState>, passphrase: String) -> Result<(), String> {
    let vault = state
        .vault_store
        .load()
        .map_err(|e| e.to_string())?
        .ok_or("no vault file")?;
    let mut session = state.server.session.lock().await;
    session
        .unlock(&vault, &passphrase)
        .map_err(|_| "incorrect passphrase or corrupt vault".to_string())?;
    drop(session);
    state.server.touch_activity(); // start the auto-lock idle clock fresh
    Ok(())
}

/// Re-lock the wallet (wipes the in-memory seed).
#[tauri::command]
async fn lock(state: State<'_, DesktopState>) -> Result<(), String> {
    state.server.session.lock().await.lock();
    Ok(())
}

/// Set the wallet's active network. Accounts (keys/addresses) are the same on
/// both networks; this changes which network operations (deploy/fund/sign) and
/// deploy-status target, and which per-network node is used.
#[tauri::command]
async fn set_network(state: State<'_, DesktopState>, network: String) -> Result<(), String> {
    let chain = match network.to_lowercase().as_str() {
        "mainnet" | "sn_main" => ChainId::Mainnet,
        "testnet" | "sepolia" | "sn_sepolia" => ChainId::Sepolia,
        other => return Err(format!("unknown network '{other}'")),
    };
    state.server.session.lock().await.set_chain(chain);
    Ok(())
}

/// Current settings (per-network RPC URLs). IPC-only; carries an endpoint that
/// may embed an API key, so it's never exposed over the HTTP service.
#[tauri::command]
async fn get_settings(state: State<'_, DesktopState>) -> Result<Settings, String> {
    Ok(settings::load(&state.config_path))
}

/// The wallet's per-network RPC URLs are the single source of truth; the prover
/// reuses them for its proof preflight. Mirror them into the prover's settings
/// (Sepolia → testnet) so both sides always hit the same node. Preserves the
/// prover's own fields (backend, remote-prover URL/key).
async fn sync_prover_rpc(prover: &ProverState, wallet: &Settings) {
    let mut ps = prover.settings.get().await;
    ps.testnet.rpc_url = wallet.sepolia_rpc.clone();
    ps.mainnet.rpc_url = wallet.mainnet_rpc.clone();
    let _ = prover.settings.update(ps).await;
}

/// Persist settings and rebuild the node client for the active network at
/// runtime (no restart needed).
#[tauri::command]
async fn set_settings(state: State<'_, DesktopState>, settings: Settings) -> Result<(), String> {
    settings.validate()?;
    settings::save(&state.config_path, &settings).map_err(|e| e.to_string())?;
    // Register a node for each network (so switchStarknetChain picks the right one).
    state
        .server
        .set_node(ChainId::Sepolia, settings.node_for(ChainId::Sepolia));
    state
        .server
        .set_node(ChainId::Mainnet, settings.node_for(ChainId::Mainnet));
    // Proving shares these RPC nodes — mirror them into the prover's settings.
    sync_prover_rpc(&state.prover, &settings).await;
    Ok(())
}

/// All accounts in the registry (empty while locked).
#[tauri::command]
async fn list_accounts(state: State<'_, DesktopState>) -> Result<Vec<AccountRef>, String> {
    let session = state.server.session.lock().await;
    Ok(session.registry().map(|r| r.accounts.clone()).unwrap_or_default())
}

/// Record a user-initiated (desktop UI) action in the shared request log, so
/// account creation / deployment show up in the Activity tab alongside agent RPC
/// calls. `decision` is "user" and the client reads "desktop (you)" to set these
/// apart from paired agents. Only locks the log (never the session), so it's safe
/// to call while a session guard is held.
async fn log_ui_action(
    state: &DesktopState,
    method: &str,
    network: &str,
    outcome: &str,
    error_code: Option<i64>,
    result_json: Option<String>,
) {
    state.server.log.lock().await.record(LogEntry {
        ts_unix_ms: wallet_rpc::now_unix_ms(),
        method: method.to_string(),
        client: Some("desktop (you)".into()),
        network: Some(network.to_string()),
        decision: "user".into(),
        outcome: outcome.to_string(),
        error_code,
        latency_ms: 0,
        params_json: None,
        result_json,
    });
}

/// STRK balance of `address` (fri, as a string to avoid JS precision loss).
/// `None` when no node is configured. Works for counterfactual (undeployed)
/// accounts — they can hold tokens before deployment.
#[tauri::command]
async fn balance(state: State<'_, DesktopState>, address: String) -> Result<Option<String>, String> {
    let chain = state.server.session.lock().await.chain();
    let node = match state.server.node_for(chain) {
        Some(n) => n,
        None => return Ok(None),
    };
    let token = wallet_core::Felt::from_hex(wallet_rpc::dispatch::STRK_TOKEN_ADDRESS)
        .map_err(|_| "bad STRK token address".to_string())?;
    let holder = wallet_core::Felt::from_hex(&address).map_err(|_| "bad address".to_string())?;
    node.balance_of(&token, &holder)
        .await
        .map(|v| Some(v.to_string()))
        .map_err(|e| e.to_string())
}

/// Derive the next user-domain account, persist the vault, return it. Rolls the
/// in-memory add back if the write fails. Logged to the Activity tab.
#[tauri::command]
async fn add_user_account(
    state: State<'_, DesktopState>,
    label: String,
) -> Result<AccountRef, String> {
    let mut session = state.server.session.lock().await;
    let network = chain_name(session.chain()).to_string();
    let account = match session.create_user_account(label) {
        Ok(a) => a,
        Err(e) => {
            let msg = e.to_string();
            log_ui_action(&state, "ui_addUserAccount", &network, &format!("error: {msg}"), Some(163), None).await;
            return Err(msg);
        }
    };
    let save = match session.reseal() {
        Ok(vault) => state.vault_store.save(&vault).map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = save {
        session.rollback_account(&account.address);
        log_ui_action(&state, "ui_addUserAccount", &network, &format!("error: {e}"), Some(163), None).await;
        return Err(e);
    }
    log_ui_action(
        &state,
        "ui_addUserAccount",
        &network,
        "ok",
        None,
        Some(format!("{{\"address\":\"{}\",\"label\":\"{}\"}}", account.address, account.label)),
    )
    .await;
    Ok(account)
}

/// Deployment status of an account: `Some(true/false)` if a node is configured,
/// `None` if not (can't tell without a node).
#[tauri::command]
async fn deploy_status(
    state: State<'_, DesktopState>,
    address: String,
) -> Result<Option<bool>, String> {
    let chain = state.server.session.lock().await.chain();
    let node = match state.server.node_for(chain) {
        Some(n) => n,
        None => return Ok(None),
    };
    let felt = wallet_core::Felt::from_hex(&address).map_err(|_| "bad address".to_string())?;
    match node.is_deployed(&felt).await {
        Ok(b) => Ok(Some(b)),
        Err(e) => Err(e.to_string()),
    }
}

/// Deploy an account: estimate the deploy fee, sign DEPLOY_ACCOUNT, and broadcast.
/// Requires a node (Settings → RPC URL) and the account to already hold funds for
/// its own deploy fee. The user clicking Deploy is the consent.
#[tauri::command]
async fn deploy_account(
    state: State<'_, DesktopState>,
    address: String,
) -> Result<serde_json::Value, String> {
    let want = {
        let f = wallet_core::Felt::from_hex(&address).map_err(|_| "bad address".to_string())?;
        format!("0x{:064x}", f)
    };
    let session = state.server.session.lock().await;
    let chain = session.chain();
    let network = chain_name(chain).to_string();
    let node = state
        .server
        .node_for(chain)
        .ok_or("set a Starknet RPC URL in Settings to deploy")?;

    let account = session
        .registry()
        .map_err(|e| e.to_string())?
        .accounts
        .iter()
        .find(|a| a.address == want)
        .cloned()
        .ok_or("unknown account")?;
    let addr_felt =
        wallet_core::Felt::from_hex(&account.address).map_err(|_| "bad address".to_string())?;

    // Small helper to log + return an error outcome to the Activity tab.
    macro_rules! fail {
        ($code:expr, $msg:expr) => {{
            let msg: String = $msg;
            log_ui_action(&state, "ui_deployAccount", &network, &format!("error: {msg}"), Some($code), None).await;
            return Err(msg);
        }};
    }

    // Pre-check: don't re-deploy an already-deployed account. A second deploy
    // reuses nonce 0 and the node rejects it with a confusing "invalid nonce"
    // (the account's nonce is already 1) — surface a clear message instead.
    match node.is_deployed(&addr_felt).await {
        Ok(true) => fail!(-32006, format!("account {} is already deployed on {network}", account.address)),
        Ok(false) => {}
        Err(e) => fail!(-32004, e.to_string()),
    }

    // Estimate the deploy fee from the deployment data.
    let d = session.deployment_data_for(&account).map_err(|e| e.to_string())?;
    let bounds = match node
        .estimate_deploy_account(&d.address, &d.class_hash, &d.constructor_calldata, &d.salt)
        .await
    {
        Ok(b) => b,
        Err(e) => fail!(-32004, e.to_string()),
    };

    let params = wallet_core::InvokeV3Params {
        nonce: wallet_core::Felt::ZERO,
        tip: 0,
        l1_gas: bounds.l1_gas,
        l2_gas: bounds.l2_gas,
        l1_data_gas: bounds.l1_data_gas,
        ..Default::default()
    };
    let signed = session
        .sign_deploy_account_for(&account, session.chain(), &params)
        .map_err(|e| e.to_string())?;

    let hash = match node
        .add_deploy_account(
            &signed.class_hash,
            &signed.constructor_calldata,
            &signed.salt,
            &[signed.r, signed.s],
            &bounds,
        )
        .await
    {
        Ok(h) => h,
        Err(e) => fail!(-32004, e.to_string()),
    };

    let tx = format!("0x{:x}", hash);
    log_ui_action(
        &state,
        "ui_deployAccount",
        &network,
        "ok",
        None,
        Some(format!("{{\"transaction_hash\":\"{tx}\",\"address\":\"{}\"}}", account.address)),
    )
    .await;
    Ok(serde_json::json!({ "transaction_hash": tx }))
}

/// Paired clients with their grant status (for the Agents control panel).
#[tauri::command]
async fn list_clients(state: State<'_, DesktopState>) -> Result<Vec<wallet_rpc::ClientInfo>, String> {
    Ok(state.server.clients.lock().await.list())
}

/// Grant a client an auto-approval window of `days` (clamped to 1..=90 = 3 months).
/// While active, that client's own-account operations skip the prompt; funding
/// always still prompts.
#[tauri::command]
async fn grant_permission(
    state: State<'_, DesktopState>,
    client_id: String,
    days: u64,
) -> Result<(), String> {
    let days = days.clamp(1, 90);
    let until = wallet_rpc::now_unix_ms() + days * 24 * 60 * 60 * 1000;
    if state.server.clients.lock().await.grant(&client_id, until) {
        Ok(())
    } else {
        Err("unknown client".into())
    }
}

/// Revoke a client's auto-approval grant immediately.
#[tauri::command]
async fn revoke_permission(state: State<'_, DesktopState>, client_id: String) -> Result<(), String> {
    if state.server.clients.lock().await.revoke_grant(&client_id) {
        Ok(())
    } else {
        Err("unknown client".into())
    }
}

/// The most recent request-log entries (newest first).
#[tauri::command]
async fn recent_log(state: State<'_, DesktopState>, limit: usize) -> Result<Vec<LogEntry>, String> {
    Ok(state.server.log.lock().await.recent(limit))
}

/// Answer a pending approval prompt. Sync (no await) — just resolves the oneshot
/// the service is blocked on.
#[tauri::command]
fn respond_approval(
    app: AppHandle,
    state: State<'_, DesktopState>,
    id: u64,
    approved: bool,
) -> Result<(), String> {
    let (tx, remaining) = {
        let mut g = state.pending.lock().unwrap();
        (g.remove(&id), g.len())
    };
    match tx {
        Some(tx) => {
            let _ = tx.send(if approved {
                Decision::Approve
            } else {
                Decision::Reject
            });
            refresh_tray(&app, remaining);
            Ok(())
        }
        None => Err("no pending approval with that id (it may have timed out)".into()),
    }
}

// ---------------------------------------------------------------------------
// IPC commands — on-device proving (Proving panel)
//
// The prover holds no key material; it proves an already-signed payload. Its
// per-network settings carry a remote-prover API key, so — like the wallet's
// settings — they are reachable over trusted IPC only, never the loopback
// service. Agents prove via `companion_prove` on the loopback service instead.
// ---------------------------------------------------------------------------

/// Prover backend kind + readiness, for the Proving panel header.
#[tauri::command]
async fn prover_status(state: State<'_, DesktopState>) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "prover": state.prover.prover.kind(),
        "ready": state.prover.prover.ready(),
        "version": env!("CARGO_PKG_VERSION"),
        // Proving is reachable to paired agents via companion_prove on the
        // loopback service (no separate open port).
        "service_url": state.service_url,
    }))
}

/// Recent proof activity (live jobs + recent terminal states), newest first.
#[tauri::command]
async fn proof_activity(state: State<'_, DesktopState>) -> Result<Vec<Activity>, String> {
    Ok(state.prover.jobs.recent_activity().await)
}

/// Current per-network prover settings (backend + remote-prover URL/keys). The
/// `rpc_url` fields are mirrored from the wallet's RPC settings (shared nodes),
/// not edited here. Trusted IPC only — these carry secrets and are never exposed
/// over the loopback service.
#[tauri::command]
async fn get_prover_settings(state: State<'_, DesktopState>) -> Result<ProverSettings, String> {
    Ok(state.prover.settings.get().await)
}

/// Replace + persist prover settings (backend + remote-prover config). RPC URLs
/// are NOT taken from the caller — proving shares the wallet's RPC nodes, so we
/// force them from the wallet config regardless of what the UI sends.
#[tauri::command]
async fn set_prover_settings(
    state: State<'_, DesktopState>,
    settings: ProverSettings,
) -> Result<(), String> {
    let mut settings = settings;
    let wallet = crate::settings::load(&state.config_path);
    settings.testnet.rpc_url = wallet.sepolia_rpc;
    settings.mainnet.rpc_url = wallet.mainnet_rpc;
    state.prover.settings.update(settings).await.map_err(|e| e.to_string())
}

/// Aggregate proof-storage usage (records + bytes).
#[tauri::command]
async fn storage_stats(state: State<'_, DesktopState>) -> Result<StorageStats, String> {
    Ok(state.prover.storage.stats())
}

/// Metadata for every generated proof, newest first (Activity list).
#[tauri::command]
async fn list_proofs(state: State<'_, DesktopState>) -> Result<Vec<ProofSummary>, String> {
    Ok(state.prover.storage.list())
}

/// Full record (payload + proof + metadata) for one proof — Activity detail view.
#[tauri::command]
async fn proof_detail(
    state: State<'_, DesktopState>,
    job_id: String,
) -> Result<Option<ProofRecord>, String> {
    Ok(state.prover.storage.get_record(&job_id))
}

/// Delete all stored payloads/proofs; returns how many records were removed.
#[tauri::command]
async fn clear_storage(state: State<'_, DesktopState>) -> Result<u64, String> {
    Ok(state.prover.storage.clear())
}

// ---------------------------------------------------------------------------
// Approval bridge: service prompts → menu-bar dialogs
// ---------------------------------------------------------------------------

/// Drain approval prompts from the service's `ChannelApprover`, surface each to
/// the UI as an `approval-request` event, and auto-reject after 60s (spec §8).
fn spawn_approval_bridge(
    app: AppHandle,
    mut rx: tokio::sync::mpsc::Receiver<PendingApproval>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Decision>>>>,
) {
    let next_id = Arc::new(AtomicU64::new(1));
    tauri::async_runtime::spawn(async move {
        while let Some(p) = rx.recv().await {
            let PendingApproval { request, respond } = p;
            let id = next_id.fetch_add(1, Ordering::Relaxed);
            let count = {
                let mut g = pending.lock().unwrap();
                g.insert(id, respond);
                g.len()
            };

            // Tell the UI to show the in-app approval dialog.
            let _ = app.emit(
                "approval-request",
                serde_json::json!({
                    "id": id,
                    "client_label": request.client_label,
                    "method": request.method,
                    "summary": request.summary,
                }),
            );

            // Alert the user even when the window is hidden in the tray. This is
            // posted from Rust (this async bridge is always alive) — NOT the
            // webview, whose JS is suspended while hidden, which is why a
            // signing request used to produce zero notification. Banner + sound
            // + a Dock-icon bounce (attention, not focus-stealing) + the tray
            // red dot / Dock badge (refresh_tray) make it impossible to miss.
            let _ = app
                .notification()
                .builder()
                .title(notif_title(&request.method))
                .body(&request.summary)
                .sound(notif_sound())
                .show();
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.request_user_attention(Some(tauri::UserAttentionType::Critical));
            }
            refresh_tray(&app, count);

            // Auto-reject if the user doesn't respond within the window.
            let pending2 = pending.clone();
            let app2 = app.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                let remaining = {
                    let mut g = pending2.lock().unwrap();
                    let existed = g.remove(&id).map(|tx| {
                        let _ = tx.send(Decision::Reject);
                    });
                    if existed.is_some() {
                        Some(g.len())
                    } else {
                        None
                    }
                };
                if let Some(remaining) = remaining {
                    refresh_tray(&app2, remaining);
                }
            });
        }
    });
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        // Closing the window hides it — the app stays in the menu bar.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .setup(|app| {
            // macOS: run as a regular app so the strkd icon shows in the Dock
            // (clicking it re-shows the window — see the Reopen handler in run()).
            // The app still lives in the menu bar via the tray; closing the
            // window only hides it.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Regular);

            // Request OS-notification permission up front, from Rust (so it does
            // not depend on the webview being awake). The approval bridge posts a
            // banner for every signing request; without permission macOS silently
            // drops them. First launch shows the system prompt; if the user misses
            // it, they can enable "strkd" under System Settings → Notifications.
            {
                use tauri_plugin_notification::PermissionState;
                let n = app.notification();
                if !matches!(n.permission_state(), Ok(PermissionState::Granted)) {
                    let _ = n.request_permission();
                }
            }

            // Data dir + file paths (spec §10).
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("no app data dir: {e}"))?;
            std::fs::create_dir_all(&data_dir)?;
            let vault_path = data_dir.join("vault.bin");
            let log_path = data_dir.join("requests.db");
            let port_lock = data_dir.join("port.lock");
            let config_path = data_dir.join("config.json");

            // Default network for v1.
            let chain = ChainId::Sepolia;
            let vault_store = VaultStore::new(&vault_path);

            // Approval bridge plumbing.
            let (approver, rx) = ChannelApprover::new(32);
            let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Decision>>>> =
                Arc::new(Mutex::new(HashMap::new()));

            // File-backed request log (full payloads on by default — spec §9).
            let log = RequestLog::open(&log_path, true).map_err(|e| format!("open log: {e}"))?;

            // If a prover bundle shipped with the app (staged by
            // scripts/stage-prover.sh into resources/prover), point the native
            // backend at it — so a packaged app proves with no external checkout.
            // Skipped if STRKD_SNIP36_BIN is already set; in dev (no bundle) this
            // is a no-op and NativeProver falls back to its sibling-checkout
            // default (only consulted when the backend is "native").
            if std::env::var("STRKD_SNIP36_BIN").is_err() {
                if let Ok(res) = app.path().resource_dir() {
                    // Tauri's exact resource layout varies by version/config, so
                    // try both the mapped dest and the glob-preserved path.
                    for prover_dir in [res.join("prover"), res.join("resources/prover")] {
                        let bin = prover_dir.join("snip36");
                        if bin.exists() {
                            std::env::set_var("STRKD_SNIP36_BIN", &bin);
                            std::env::set_var("STRKD_SNIP36_WORK_DIR", &prover_dir);
                            break;
                        }
                    }
                }
            }

            // On-device proving companion. Its own data subdir (settings.json +
            // storage/), kept separate from the wallet vault/log. Holds no key
            // material — it proves already-signed payloads. The same Arc is both
            // attached to the loopback service (companion_prove*) and handed to
            // the desktop's Proving panel.
            let prover_state =
                Arc::new(build_prover_state(data_dir.join("prover"), &ProverConfig::from_env()));

            // Session starts locked; onboarding/unlock populates it.
            let session_arc = Arc::new(tokio::sync::Mutex::new(WalletSession::new_locked(chain)));
            let mut state = ServerState::with_log(session_arc, Arc::new(approver), log)
                .with_vault_store(vault_store.clone())
                .with_clients_path(data_dir.join("clients.json"));
            // Nodes (broadcast + fee estimation) come from saved settings
            // (Settings tab → config.json), one per network. Changeable at runtime
            // via set_settings — no restart. Sign-only until an RPC URL is set.
            let saved = settings::load(&config_path);
            // Proving shares the wallet's RPC nodes — seed the prover's settings
            // from the wallet config at startup (kept in sync on every save).
            tauri::async_runtime::block_on(sync_prover_rpc(&prover_state, &saved));
            if let Some(node) = saved.node_for(ChainId::Sepolia) {
                state = state.with_node(ChainId::Sepolia, node);
            }
            if let Some(node) = saved.node_for(ChainId::Mainnet) {
                state = state.with_node(ChainId::Mainnet, node);
            }
            // Expose proving over the loopback service (companion_prove*).
            state = state.with_prover(prover_state.clone());
            let server = Arc::new(state);

            // Start the loopback service (binds 127.0.0.1, writes port.lock).
            let (addr, _handle) = tauri::async_runtime::block_on(bind_loopback(
                server.clone(),
                Some(port_lock.as_path()),
            ))
            .map_err(|e| format!("bind service: {e}"))?;
            let service_url = format!("http://{addr}");

            // Surface approval prompts as menu-bar dialogs.
            spawn_approval_bridge(app.handle().clone(), rx, pending.clone());

            // Auto-lock after inactivity (spec §5.3). Activity = an RPC request or
            // an unlock; the UI's status polling doesn't count. Timeout from
            // Settings (auto_lock_minutes; 0 = never). A busy agent under a grant
            // keeps the session unlocked; set 0 for fully-unattended agents.
            {
                let lock_server = server.clone();
                let cfg = config_path.clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        let mins = settings::load(&cfg).auto_lock_minutes;
                        if mins == 0 {
                            continue;
                        }
                        let idle = wallet_rpc::now_unix_ms()
                            .saturating_sub(lock_server.last_activity_ms());
                        if idle >= mins * 60 * 1000 {
                            let mut s = lock_server.session.lock().await;
                            if !s.is_locked() {
                                s.lock();
                            }
                        }
                    }
                });
            }

            app.manage(DesktopState {
                server,
                vault_store,
                config_path,
                chain,
                service_url,
                pending,
                onboarding: Mutex::new(None),
                prover: prover_state,
            });

            // Menu-bar tray.
            let open = MenuItemBuilder::with_id("open", "Open strkd").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&open, &quit]).build()?;
            let mut tray = TrayIconBuilder::with_id(TRAY_ID)
                .tooltip("strkd — Starknet wallet companion")
                .menu(&menu);
            if let Some(icon) = tray_image(false) {
                tray = tray.icon(icon);
            }
            let tray = tray
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "quit" => app.exit(0),
                    "open" => show_main(app),
                    _ => {}
                })
                .build(app)?;
            app.manage(tray);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            status,
            generate,
            import,
            finalize_setup,
            unlock,
            lock,
            set_network,
            get_settings,
            set_settings,
            list_accounts,
            add_user_account,
            balance,
            deploy_status,
            deploy_account,
            list_clients,
            grant_permission,
            revoke_permission,
            recent_log,
            respond_approval,
            prover_status,
            proof_activity,
            get_prover_settings,
            set_prover_settings,
            storage_stats,
            list_proofs,
            proof_detail,
            clear_storage
        ])
        .build(tauri::generate_context!())
        .expect("error while building strkd desktop")
        .run(|_app, _event| {
            // Clicking the Dock icon while the window is hidden re-shows it.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event {
                show_main(_app);
            }
        });
}
