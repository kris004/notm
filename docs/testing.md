# Testing

Fixture tests create a disposable Maildir and Notmuch database through
`libnotmuch`. Normal tests and fixture behavior should not shell out to the
`notmuch` CLI. Desktop UI smoke tests start an isolated headless display when
Sway is available and otherwise skip with a clear reason; interactive GTK flows
can be driven through the local developer test harness described in
[automation/README.md](automation/README.md).

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

The Cargo desktop UI smokes use a private, software-rendered headless Sway
compositor by default. Each fixture app gets its own 1920x1080 Wayland display,
and at most two GUI fixtures run concurrently. This keeps test windows off the
interactive desktop and prevents unrelated fixtures from being tiled together.
`sway` must be available in `PATH`; tests skip with a clear reason when it is
missing, or fail when `NOTM_REQUIRE_GTK_DISPLAY=1` is set. For intentional
interactive debugging, set `NOTM_GUI_TEST_DISPLAY=provided` to use the existing
`WAYLAND_DISPLAY` or `DISPLAY`. CI uses this mode with its private Xvfb server,
explicitly removing inherited Wayland variables and selecting the GTK X11
backend so Sway is not a CI dependency and a local reproduction cannot reach an
interactive Wayland compositor. The older `live` value remains an alias for
`provided`.

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
2. `run_search` for `tag:inbox` reports `scheduled: true` without waiting for
   Notmuch.
3. Poll `search_status` until `loading` is false and confirm `error` is null.
4. `thread_page_info` reports loaded fixture rows.

When a GUI smoke sends a message, `compose_send` returning `pending: true`
means the composed snapshot was queued, not that sending finished. Poll
`app_state.state.send_in_progress` until it becomes false before checking
`last_send_report`, `last_error`, or send-related file changes.

For a narrowly scoped responsiveness check, fixture harness requests may pass
`test_delay_ms` to `run_search` (maximum 5000). The delay runs on the search
worker, so `health` and `search_status` must remain responsive while the search
is outstanding. This argument is rejected outside fixture harness mode.

For a bug fix, capture or reproduce the symptom first, then rerun the same
fixture/live path after the change and confirm the symptom is gone. Rust compile,
Clippy, and unit tests are still useful, but they do not replace a runtime smoke
check for UI warnings or behavior regressions.

The Settings dialog has two named, fixture-backed GTK smokes. Required-display
mode turns a missing display into a failure instead of a skip:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 CARGO_HOME=$PWD/.cargo-home \
  cargo test -p notm-app --test desktop_ui_smoke \
  fixture_settings_preview_limits_apply_without_partial_persistence \
  -- --exact --nocapture --test-threads=1
NOTM_REQUIRE_GTK_DISPLAY=1 CARGO_HOME=$PWD/.cargo-home \
  cargo test -p notm-app --test desktop_ui_smoke \
  fixture_theme_modes_follow_both_simulated_system_preferences \
  -- --exact --nocapture --test-threads=1
```

The first drives the real dialog responses for one-line, three-line, and hidden
previews and verifies that invalid values do not mutate runtime or persisted
state. The second starts separate deterministic system-light and system-dark
processes, exercises System/Light/Dark plus Save, and checks the resolved GTK
theme background rather than trusting the requested enum.

The draft confirmation flow also has a named, fixture-backed GTK smoke. It
drives the real modal through `pending_confirmation` and
`respond_confirmation`, covers reject and accept paths, blocks harness
mutations while a modal is pending, and compares compose, active-draft,
recovery-file, and persisted-draft state across a rejection. Those controls are
fixture-only except for the narrowly gated saved-draft Send flow documented in
`docs/automation/README.md`:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 CARGO_HOME=$PWD/.cargo-home \
  cargo test -p notm-app --test desktop_ui_smoke \
  fixture_draft_confirmations_preserve_rejected_state \
  -- --exact --nocapture --test-threads=1
```

Current-message navigation and message-only tag actions have a separate
fixture-backed smoke. It verifies bounded relative navigation, drives the real
custom-tag button, proves that only the selected message ID changes, and checks
exact undo restoration:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 CARGO_HOME=$PWD/.cargo-home \
  cargo test -p notm-app --test desktop_ui_smoke \
  fixture_current_message_navigation_and_tagging_are_explicit \
  -- --exact --nocapture --test-threads=1
```

Visual-HTML link hints have a fixture-backed GTK smoke that verifies visible
links receive distinct labels and that cancelling clears the mode:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 CARGO_HOME=$PWD/.cargo-home \
  cargo test -p notm-app --test desktop_ui_smoke \
  fixture_html_link_hints_label_visible_links_and_cancel \
  -- --exact --nocapture --test-threads=1
```

Vim-style message viewport scrolling has a long-HTML GTK smoke. It routes
`Ctrl+e` and `Ctrl+y` through the main shortcut router, verifies movement in
both directions, and proves the selected message does not change:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 CARGO_HOME=$PWD/.cargo-home \
  cargo test -p notm-app --test desktop_ui_smoke \
  fixture_ctrl_e_y_scroll_message_without_changing_selection \
  -- --exact --nocapture --test-threads=1
```

Remembered message views have a restart-backed GTK smoke. It verifies all
preference layers, drives the real sender button, checks Message-ID precedence,
and confirms that standalone windows resolve their own selected message:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 CARGO_HOME=$PWD/.cargo-home \
  cargo test -p notm-app --test desktop_ui_smoke \
  fixture_message_and_sender_views_persist_with_message_precedence \
  -- --exact --nocapture --test-threads=1
```

Use the fixture-only test-harness `send_key` command for application shortcut
checks that do not need compositor input; it calls the same ordered router as
the main window without focusing or presenting a window. The focused-text and
physical-key propagation regressions retain a self-contained headless Sway
check. It covers J/K message navigation, lowercase j/k scrolling, the M
current-message menu and its two-key actions, physical Shift+F routing to link
hints (with overlay behavior covered by the GTK smoke above), and
normal/insert-mode tag-editor safety. It requires
`dbus-run-session`, `sway`, `swaymsg`, and `wtype`:

```sh
CARGO_HOME=$PWD/.cargo-home cargo build -p notm-app
python3 -B tests/ui_text_focus_smoke.py --binary target/debug/notm
```

Test-harness reports and screenshots are local validation artifacts under
`artifacts/`; they are ignored by git except for `artifacts/logs/.gitkeep`.
Keep one-off progress reports and completed planning notes out of the public
documentation tree.
