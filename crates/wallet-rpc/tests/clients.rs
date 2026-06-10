//! Client-store persistence: pairings + grants survive a reopen (a restart),
//! and only token hashes are written to disk.

use wallet_rpc::{ClientKind, ClientStore};

#[test]
fn pairings_and_grants_persist_across_reopen() {
    let dir = std::env::temp_dir().join(format!("strkd-clients-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clients.json");

    let (id, token) = {
        let mut store = ClientStore::open(path.clone());
        let (id, token) = store.pair("bot", ClientKind::Agent);
        // Grant a far-future window.
        let until = wallet_rpc::now_unix_ms() + 60 * 24 * 60 * 60 * 1000;
        assert!(store.grant(&id, until));
        (id, token)
    }; // store dropped — simulates app exit

    // Reopen (a "restart").
    let reopened = ClientStore::open(path.clone());
    // The agent's existing token still verifies.
    let client = reopened.verify(&token).expect("token still valid after restart");
    assert_eq!(client.id, id);
    assert_eq!(client.kind, ClientKind::Agent);
    // The grant survived and is active.
    assert!(client.grant_active(wallet_rpc::now_unix_ms()));

    // The on-disk file holds the hash, NOT the raw token.
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains(&token), "raw token must never be persisted");
    assert!(raw.contains("token_hash"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn revoked_grant_persists_as_revoked() {
    let dir = std::env::temp_dir().join(format!("strkd-clients2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clients.json");

    let token = {
        let mut store = ClientStore::open(path.clone());
        let (id, token) = store.pair("bot", ClientKind::Agent);
        store.grant(&id, wallet_rpc::now_unix_ms() + 1_000_000_000);
        store.revoke_grant(&id);
        token
    };

    let reopened = ClientStore::open(path.clone());
    let client = reopened.verify(&token).unwrap();
    assert!(!client.grant_active(wallet_rpc::now_unix_ms()), "revoke persisted");

    let _ = std::fs::remove_dir_all(&dir);
}
