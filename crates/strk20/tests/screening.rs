//! Screening attestations against starknet-privacy's cross-language vectors.
//!
//! `fixtures/screening-vectors.json` is copied verbatim from
//! `starkware-libs/starknet-privacy` (`fixtures/screening-vectors.json` at the
//! deployed class's commit `4026da7`). The pool's Cairo tests, its Python
//! generator and its TypeScript test signer all reproduce it. The signer key is
//! the public test key `0xCAFEBABE`, never a real screener's.

use serde_json::Value;
use starknet_types_core::felt::Felt;
use strk20::screening;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/screening-vectors.json");

fn f(v: &Value) -> Felt {
    Felt::from_hex(v.as_str().unwrap()).unwrap()
}

#[test]
fn reproduces_every_screening_vector() {
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(FIXTURE).unwrap()).unwrap();
    let sk = f(&fixture["signer_private_key"]);
    let pk = f(&fixture["signer_public_key"]);
    assert_eq!(wallet_core::public_key_of_key(&sk).unwrap(), pk);

    let vectors = fixture["vectors"].as_array().unwrap();
    assert_eq!(vectors.len(), 3);
    for v in vectors {
        let name = v["name"].as_str().unwrap();
        let chain_id = f(&v["chain_id"]);
        let depositor = f(&v["depositor"]);
        let issued_at = v["issued_at"].as_u64().unwrap();

        let hash = screening::message_hash(chain_id, depositor, issued_at, pk);
        assert_eq!(hash, f(&v["message_hash"]), "{name}: message hash");

        // RFC 6979 is deterministic, so strkd's signature is the vector's.
        let a = screening::attest(&sk, chain_id, depositor, issued_at).unwrap();
        assert_eq!(a.issued_at, issued_at, "{name}");
        assert_eq!((a.sig_r, a.sig_s), (f(&v["sig_r"]), f(&v["sig_s"])), "{name}: signature");
    }
}

#[test]
fn rejects_an_unusable_screener_key() {
    assert!(screening::attest(&Felt::ZERO, Felt::ONE, Felt::TWO, 1).is_err());
}
