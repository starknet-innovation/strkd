//! Tests for the SQLite-backed request log: ordering, the full-payload
//! redaction toggle, and file persistence across reopen.

use wallet_rpc::{now_unix_ms, LogEntry, RequestLog};

fn entry(method: &str) -> LogEntry {
    LogEntry {
        ts_unix_ms: now_unix_ms(),
        method: method.into(),
        client: Some("app (client-1)".into()),
        network: Some("SN_SEPOLIA".into()),
        decision: "approved".into(),
        outcome: "ok".into(),
        error_code: None,
        latency_ms: 1,
        params_json: Some(r#"{"foo":"bar"}"#.into()),
        result_json: Some(r#"["0x1","0x2"]"#.into()),
    }
}

#[test]
fn records_and_reads_back_newest_first() {
    let log = RequestLog::in_memory(true).unwrap();
    log.record(entry("a"));
    log.record(entry("b"));
    log.record(entry("c"));
    assert_eq!(log.count(), 3);
    let recent = log.recent(2);
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].method, "c"); // newest first
    assert_eq!(recent[1].method, "b");
}

#[test]
fn full_payloads_on_keeps_params_and_result() {
    let log = RequestLog::in_memory(true).unwrap();
    log.record(entry("wallet_signTypedData"));
    let e = &log.recent(1)[0];
    assert!(e.params_json.is_some());
    assert!(e.result_json.is_some());
}

#[test]
fn full_payloads_off_redacts_params_and_result_but_keeps_summary() {
    let log = RequestLog::in_memory(false).unwrap();
    log.record(entry("wallet_signTypedData"));
    let e = &log.recent(1)[0];
    // Redacted:
    assert!(e.params_json.is_none());
    assert!(e.result_json.is_none());
    // Summary retained:
    assert_eq!(e.method, "wallet_signTypedData");
    assert_eq!(e.client.as_deref(), Some("app (client-1)"));
    assert_eq!(e.decision, "approved");
    assert_eq!(e.outcome, "ok");
}

#[test]
fn toggle_applies_to_subsequent_records() {
    let mut log = RequestLog::in_memory(true).unwrap();
    log.record(entry("first")); // full
    log.set_full_payloads(false);
    log.record(entry("second")); // redacted
    let recent = log.recent(2); // [second, first]
    assert!(recent[0].params_json.is_none()); // second redacted
    assert!(recent[1].params_json.is_some()); // first kept
}

#[test]
fn file_backed_log_persists_across_reopen() {
    let dir = std::env::temp_dir().join(format!("strkd-log-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("requests.db");

    {
        let log = RequestLog::open(&path, true).unwrap();
        log.record(entry("persisted_method"));
        assert_eq!(log.count(), 1);
    } // close

    // Reopen: the row is still there.
    let reopened = RequestLog::open(&path, true).unwrap();
    assert_eq!(reopened.count(), 1);
    assert_eq!(reopened.recent(1)[0].method, "persisted_method");

    let _ = std::fs::remove_dir_all(&dir);
}
