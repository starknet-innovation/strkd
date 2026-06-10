//! Vault persistence: agent-account creation re-seals and writes the vault, and
//! the change survives a reopen + unlock cycle. Public test vector only.

use std::sync::Arc;

use serde_json::json;
use tokio::sync::Mutex;
use wallet_core::{ChainId, EncryptedVault, Registry};
use wallet_rpc::{
    dispatch, AutoApprover, Decision, Request, Response, ServerState, VaultStore, WalletSession,
};

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const PASS: &str = "vault-pass";

async fn call(state: &ServerState, token: Option<&str>, method: &str, params: serde_json::Value) -> Response {
    dispatch(
        state,
        token,
        Request {
            jsonrpc: Some("2.0".into()),
            method: method.into(),
            params,
            id: json!(1),
        },
    )
    .await
}

async fn pair_agent(state: &ServerState) -> String {
    let resp = call(
        state,
        None,
        "companion_requestPairing",
        json!({"name": "bot", "kind": "agent"}),
    )
    .await;
    resp.result.unwrap()["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn agent_account_creation_persists_to_disk_and_survives_reopen() {
    let dir = std::env::temp_dir().join(format!("strkd-persist-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let vault_path = dir.join("vault.bin");

    // Seed an initial vault file with the mnemonic and an empty registry.
    let initial = WalletSession::new_unlocked(
        ChainId::Sepolia,
        TEST_MNEMONIC,
        PASS,
        Registry::default(),
    );
    let store = VaultStore::new(&vault_path);
    store.save(&initial.reseal().unwrap()).unwrap();
    assert!(store.exists());

    // Build a server state backed by that vault file.
    let session = WalletSession::new_unlocked(ChainId::Sepolia, TEST_MNEMONIC, PASS, Registry::default());
    let state = Arc::new(
        ServerState::new(
            Arc::new(Mutex::new(session)),
            Arc::new(AutoApprover(Decision::Approve)),
        )
        .with_vault_store(store.clone()),
    );

    // Agent pairs and creates an account.
    let token = pair_agent(&state).await;
    let created = call(&state, Some(&token), "companion_createAgentAccount", json!({"label": "savings"})).await;
    let acct = created.result.expect("account created");
    let created_addr = acct["address"].as_str().unwrap().to_string();

    // The vault file on disk now reflects the new account: reopen, decrypt,
    // and confirm the registry contains it.
    let reopened: EncryptedVault = store.load().unwrap().expect("vault present");
    let mut fresh = WalletSession::new_locked(ChainId::Sepolia);
    fresh.unlock(&reopened, PASS).unwrap();
    let reg = fresh.registry().unwrap();
    assert!(
        reg.accounts.iter().any(|a| a.address == created_addr),
        "persisted vault must contain the newly created agent account"
    );
    assert_eq!(reg.accounts.len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn wrong_passphrase_cannot_open_persisted_vault() {
    let dir = std::env::temp_dir().join(format!("strkd-persist2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let vault_path = dir.join("vault.bin");
    let store = VaultStore::new(&vault_path);

    let session = WalletSession::new_unlocked(ChainId::Sepolia, TEST_MNEMONIC, PASS, Registry::default());
    store.save(&session.reseal().unwrap()).unwrap();

    let vault = store.load().unwrap().unwrap();
    let mut s = WalletSession::new_locked(ChainId::Sepolia);
    assert!(s.unlock(&vault, "wrong-pass").is_err());
    assert!(s.is_locked());

    let _ = std::fs::remove_dir_all(&dir);
}
