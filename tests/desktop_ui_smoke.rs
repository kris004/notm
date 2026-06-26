#[test]
fn desktop_ui_smoke_is_environment_gated() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipping desktop UI smoke: no DISPLAY/WAYLAND_DISPLAY in test environment");
    }
    // Full interactive test-harness smoke is run manually via notm launch --fixture --test-harness.
}
