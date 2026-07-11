use std::{ffi::OsStr, process::Command};

#[test]
#[ignore = "live send: set NOTM_RUN_LIVE_SEND_TEST=1 and run this test with --ignored --exact"]
fn live_self_send_runs_real_command() {
    assert!(
        std::env::var_os("NOTM_RUN_LIVE_SEND_TEST").as_deref() == Some(OsStr::new("1")),
        "refusing to send live mail: set NOTM_RUN_LIVE_SEND_TEST=1 explicitly"
    );

    let status = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["run", "--quiet", "-p", "notm-app", "--", "live-self-send"])
        .status()
        .expect("failed to start Cargo for the live self-send smoke");

    assert!(
        status.success(),
        "`notm live-self-send` failed with {status}"
    );
}
