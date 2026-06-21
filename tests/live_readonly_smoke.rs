#[test]
fn live_readonly_smoke_is_explicitly_gated() {
    if std::env::var_os("NOTM_RUN_LIVE_TESTS").is_none() {
        eprintln!(
            "skipping live readonly smoke in cargo test; run `cargo run -p notm-app -- live-readonly-smoke`"
        );
    }
}
