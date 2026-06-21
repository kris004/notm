#[test]
fn live_self_send_is_explicitly_gated() {
    if std::env::var_os("NOTM_RUN_LIVE_SEND_TEST").is_none() {
        eprintln!(
            "skipping live self-send in cargo test; run `cargo run -p notm-app -- live-self-send` for the one-shot send"
        );
    }
}
