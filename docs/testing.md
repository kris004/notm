# Testing

Fixture tests create a disposable Maildir and Notmuch database through libnotmuch. No fixture or normal test shells out to the `notmuch` CLI. Desktop UI smoke tests skip with a clear reason when no GTK display is available; live GTK gates are driven through the local automation socket.

## Final gate commands

```sh
CARGO_HOME=$PWD/.cargo-home cargo fmt --all -- --check
CARGO_HOME=$PWD/.cargo-home cargo clippy --workspace --all-targets -- -D warnings
CARGO_HOME=$PWD/.cargo-home cargo test --workspace
CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- fixture-smoke
CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- probe-send
CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- live-readonly-smoke
```

Final validation output is saved in `artifacts/reports/final-full-validation.txt`.

## GUI/automation gates

The actual GTK app was launched and driven with automation for fixture and live flows. Key reports:

- `artifacts/reports/gap1-live-ui-send-summary.json`
- `artifacts/reports/gap2-summary.json`
- `artifacts/reports/gap3-summary.json`
- `artifacts/reports/gap4-summary.json`
- `artifacts/reports/gap5-summary.json`
- `artifacts/reports/gap6-summary.json`
- `artifacts/reports/final-gap7-ui-summary.json`
- `artifacts/reports/final-ui-post-sync-polish-summary.json`
- `artifacts/reports/large-inbox-paging-summary.json`
- `artifacts/reports/webkit-html-visual-summary.json`
- `artifacts/reports/image-policy-scroll-summary.json`

Key screenshots are in `artifacts/screenshots/`, including `01_app_start.png` through `31_auto_load_more_scroll.png`.

The one-shot live self-send gate was completed earlier in the run and later live UI validation sent two bounded messages with prefix `notm validation self-test`; no sync command was run.
