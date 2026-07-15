# Testing

Fixture tests create a disposable Maildir and Notmuch database through
`libnotmuch`. Normal tests and fixture behavior should not shell out to the
`notmuch` CLI. Desktop UI smoke tests skip with a clear reason when no GTK
display is available; interactive GTK flows can be driven through the local
developer test harness described in [automation/README.md](automation/README.md).

## Routine checks

```sh
CARGO_HOME=$PWD/.cargo-home cargo fmt --all -- --check
CARGO_HOME=$PWD/.cargo-home cargo clippy --workspace --all-targets --all-features -- -D warnings
CARGO_HOME=$PWD/.cargo-home cargo test --workspace --all-targets --all-features
CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- fixture-smoke
CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- probe-send
```

`probe-send` checks the configured transport behavior without sending mail.

## Live smoke commands

Live smoke commands use the user's configured Notmuch database and/or send
transport. Run them only when that is intentional.

```sh
CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- live-readonly-smoke
CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- live-self-send
```

`live-readonly-smoke` opens the configured Notmuch database read-only and runs
the configured default query. `live-self-send` sends one unique message through
the configured transport and then waits briefly for it to appear in Notmuch; it
does not force sync.

## GUI smoke checks

Use fixture data first when validating UI behavior. Start the app with the local
developer test harness:

```sh
cargo run -p notm-app -- launch --fixture \
  --test-harness \
  --test-harness-socket /tmp/notm.sock \
  --test-harness-token dev-token
```

Fixture launch replaces account identity and data paths with disposable fixture
values. It never runs configured receive/database-update commands or an
external send helper; fixture sends are captured locally and tag changes apply
only to the disposable database.

For a non-fixture harness, sending and tag mutation are denied unless
`automation.allow_live_send_test` or `automation.allow_live_tag_test` is
explicitly enabled, respectively. The same gates apply when a mutating action
is reached through `run_command`.

Then drive the same path the change is meant to affect. A basic smoke check is:

1. `health` responds with `ok: true`.
2. `run_search` for `tag:inbox` succeeds.
3. `thread_page_info` reports loaded fixture rows.

For a bug fix, capture or reproduce the symptom first, then rerun the same
fixture/live path after the change and confirm the symptom is gone. Rust compile,
Clippy, and unit tests are still useful, but they do not replace a runtime smoke
check for UI warnings or behavior regressions.

The focused-text shortcut regression has a self-contained headless Sway check.
It requires `dbus-run-session`, `sway`, `swaymsg`, and `wtype`:

```sh
CARGO_HOME=$PWD/.cargo-home cargo build -p notm-app
python3 -B tests/ui_text_focus_smoke.py --binary target/debug/notm
```

Test-harness reports and screenshots are local validation artifacts under
`artifacts/`; they are ignored by git except for `artifacts/logs/.gitkeep`. Keep
long progress reports out of the root README and archive completed planning notes
under `docs/archive/` when they are no longer current.
