# Testing

Fixture tests create a disposable Maildir and Notmuch database through
`libnotmuch`. Normal tests and fixture behavior should not shell out to the
`notmuch` CLI. During an ordinary `cargo test --locked` run, desktop UI smoke
tests start an isolated headless display when Sway is available and may
otherwise skip with a clear reason. The complete delivery gate below supplies
required Weston and Xvfb displays; `NOTM_REQUIRE_GTK_DISPLAY=1` makes an
unavailable display fail, so a skipped required-display test never counts as a
gate pass.
Interactive GTK flows can be driven through the local developer test harness
described in [automation/README.md](automation/README.md).

## Routine checks

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
make smoke
make check-packaging
```

`make smoke` is fixture-only. `make check-packaging` runs the lock-policy and
mutation regressions, release metadata and signing-key policy tests, staged
install checks, deterministic release-bundle build and verification, and
disposable signed-tag verification.

To exercise `probe-send` without depending on a contributor's mail setup, use a
disposable helper configuration:

```sh
probe_config=$(mktemp)
trap 'rm -f "$probe_config"' EXIT
cat >"$probe_config" <<'EOF'
[send]
enabled = true
transport = "external"
command = "true"
mode = "stdin_rfc5322"
EOF
cargo run --locked -p notm-app -- --config "$probe_config" probe-send
```

The probe resolves the command and checks its working directory; it does not
submit a message.

## Complete hermetic delivery gate

The delivery gate is intentionally explicit. It requires Cargo and the native
build dependencies plus `actionlint`, ShellCheck, mandoc,
`desktop-file-validate`, `appstreamcli`, GnuPG, Weston, Xvfb, Sway, `wtype`,
`dbus-run-session`, and Python 3. A missing tool, skipped display test, or
unavailable command is a failure, not a pass.

Run the routine checks and the disposable send probe above, then:

```sh
./tests/source_archive_smoke.sh

tests/run_with_headless_weston.sh \
  dbus-run-session -- \
  cargo test --locked -p notm-app --test desktop_ui_smoke -- \
    --nocapture --test-threads=1

env -u DISPLAY -u WAYLAND_DISPLAY -u SWAYSOCK \
  GDK_BACKEND=x11 \
  NOTM_GUI_TEST_DISPLAY=provided \
  NOTM_REQUIRE_GTK_DISPLAY=1 \
  dbus-run-session -- \
  xvfb-run -a \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
    fixture_html_link_hints_label_visible_links_and_cancel -- \
    --exact --nocapture --test-threads=1

cargo build --locked -p notm-app
python3 -B tests/ui_text_focus_smoke.py --binary target/debug/notm

actionlint
shellcheck packaging/*.sh tests/*.sh
mandoc -Tlint \
  docs/man/notm.1 \
  docs/man/notm-config.5 \
  docs/man/notm-test-harness.7 \
  docs/man/notm-automation.7
desktop-file-validate packaging/io.github.kris004.notm.desktop
appstreamcli validate --strict --pedantic --no-net \
  packaging/io.github.kris004.notm.metainfo.xml
```

`tests/source_archive_smoke.sh` creates the exact source-archive form, confirms
that it has no `.git`, verifies embedded commit and version provenance, and
runs a clean locked release build, workspace test, fixture smoke, and packaging
suite from the extraction. The full workspace/all-target/all-feature test run
uses the standard Rust harness with `--test-threads=1` so GUI-capable tests keep
deterministic compositor isolation. The packaging suite includes the deliberate
Cargo.lock-mutation negative test and the standalone release-bundle verifier.

## Live smoke commands

Live smoke commands use the user's configured Notmuch database and/or send
transport. Run them only when that is intentional.

```sh
make smoke-live-readonly
make smoke-live-send
```

`live-readonly-smoke` opens the configured Notmuch database read-only and runs
the configured default query. `live-self-send` sends one unique message through
the configured transport and then waits briefly for it to appear in Notmuch; it
does not force sync.

## Sync checks

The sync unit coverage verifies manual/startup gates, receive-before-update
ordering, selected `NOTMUCH_*` environment propagation, nonzero-exit handling,
bounded diagnostics, and timeout cleanup:

```sh
cargo test --locked -p notm-ui main_window::tests::sync_ --lib -- --nocapture
```

Four required-display smokes exercise the real GTK startup/manual actions,
responsiveness, post-sync refresh, failure recovery, and application lifetime:

```sh
for test in \
  slow_manual_sync_keeps_desktop_responsive \
  startup_sync_runs_receive_then_database_update \
  failed_manual_sync_reports_stderr_and_recovers \
  closing_main_window_waits_for_manual_sync
do
  NOTM_REQUIRE_GTK_DISPLAY=1 \
    cargo test --locked -p notm-app --test desktop_ui_smoke "$test" -- \
      --exact --nocapture --test-threads=1
done
```

These tests use disposable helpers and databases. Live fetch commands are never
required.

## GUI smoke checks

The Cargo desktop UI smokes use a private, software-rendered headless Sway
compositor by default. Each fixture app gets its own 1920x1080 Wayland display,
and at most two GUI fixtures run concurrently. This keeps test windows off the
interactive desktop and prevents unrelated fixtures from being tiled together.
`sway` must be available in `PATH`; tests skip with a clear reason when it is
missing, or fail when `NOTM_REQUIRE_GTK_DISPLAY=1` is set. For intentional
interactive debugging, set `NOTM_GUI_TEST_DISPLAY=provided` to use the existing
`WAYLAND_DISPLAY` or `DISPLAY`. CI runs the suite through
`tests/run_with_headless_weston.sh`, which starts a private software-rendered
Weston display with a native Wayland GTK backend. This makes Sway unnecessary
in CI and prevents a local CI reproduction from reaching the interactive
compositor. CI also runs the link-hint fixture under Xvfb as a narrow GTK and
WebKitGTK X11-backend check without repeating the complete UI suite. The older
`live` value remains an alias for `provided`.

Use fixture data first when validating UI behavior. Start the app with the local
developer test harness:

```sh
cargo run --locked -p notm-app -- launch --fixture \
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

The Settings dialog has three named, fixture-backed GTK smokes. Required-display
mode turns a missing display into a failure instead of a skip:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
  fixture_settings_preview_limits_apply_without_partial_persistence \
  -- --exact --nocapture --test-threads=1
NOTM_REQUIRE_GTK_DISPLAY=1 \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
  fixture_theme_modes_follow_both_simulated_system_preferences \
  -- --exact --nocapture --test-threads=1
NOTM_REQUIRE_GTK_DISPLAY=1 \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
  fixture_send_timeout_validation_preserves_last_valid_value_across_restart \
  -- --exact --nocapture --test-threads=1
```

The first drives the real dialog responses for one-line, three-line, and hidden
previews and verifies that invalid values do not mutate runtime or persisted
state. The second starts separate deterministic system-light and system-dark
processes, exercises System/Light/Dark plus Save, and checks the resolved GTK
theme background rather than trusting the requested enum. The third rejects
zero, negative, nonnumeric, and overflowing send timeouts without changing the
config, saves the maximum valid value, then restarts from those exact persisted
TOML bytes and verifies that the timeout is loaded and shown.

The draft confirmation flow also has a named, fixture-backed GTK smoke. It
drives the real modal through `pending_confirmation` and
`respond_confirmation`, covers reject and accept paths, blocks harness
mutations while a modal is pending, and compares compose, active-draft,
recovery-file, and persisted-draft state across a rejection. Those controls are
fixture-only except for the narrowly gated saved-draft Send flow documented in
`docs/automation/README.md`:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
  fixture_draft_confirmations_preserve_rejected_state \
  -- --exact --nocapture --test-threads=1
```

A separate non-fixture smoke uses a disposable Notmuch database and Maildir to
verify first and repeated indexed draft saves, the background search refresh,
and clean navigation away from the saved composer:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
  indexed_maildir_draft_refresh_stays_clean_during_message_navigation \
  -- --exact --nocapture --test-threads=1
```

A restart-backed variant verifies that a clean indexed save removes transient
recovery state, survives a normal process exit, and reopens from `tag:draft`
without an unsaved-composer prompt:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
  indexed_maildir_saved_draft_restart_does_not_prompt_as_unsaved \
  -- --exact --nocapture --test-threads=1
```

Outbound wire interoperability has a required-display, clean-XDG E2E. It uses
only a disposable Notmuch database, `.test` addresses, a Python submission
helper, and a Rust SMTP capture server bound to `127.0.0.1` on an ephemeral
port. The test composes, saves, restarts, reopens, replies, forwards as an
attachment, and sends Unicode/long-header/Bcc messages. Python's independent
standard-library MIME parser then checks the exact captured bytes, including
envelope recipients, Bcc removal, folding, CRLF and line limits, threading,
alternatives, and attachment hashes:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 \
WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 \
tests/run_with_headless_weston.sh \
  dbus-run-session -- \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
    clean_xdg_local_smtp_wire_interoperability -- \
    --exact --nocapture --test-threads=1
```

This smoke must not be redirected to a real account, SMTP relay, or installed
sendmail implementation. A skip is not a pass; keep
`NOTM_REQUIRE_GTK_DISPLAY=1` set for delivery validation.

Indexed-draft deletion has a fixture-backed regression that confirms the row
is removed, the file is deleted, and the message pane never attempts to render
the now-missing body:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
  fixture_indexed_draft_delete_removes_row_without_missing_body \
  -- --exact --nocapture --test-threads=1
```

Current-message navigation and message-only tag actions have a separate
fixture-backed smoke. It verifies bounded relative navigation, drives the real
custom-tag button, proves that only the selected message ID changes, and checks
exact undo restoration:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
  fixture_current_message_navigation_and_tagging_are_explicit \
  -- --exact --nocapture --test-threads=1
```

Visual-HTML link hints have a fixture-backed GTK smoke that verifies visible
links receive distinct labels and that cancelling clears the mode:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
  fixture_html_link_hints_label_visible_links_and_cancel \
  -- --exact --nocapture --test-threads=1
```

Remote-image privacy has an indexed, clean-XDG GTK smoke under a private Weston
Wayland compositor and D-Bus session. It starts a temporary loopback HTTP
tracker and verifies default blocking, exactly one selected-message load,
spoofed and malformed `From:` isolation, navigation and restart reset,
legacy-entry retirement, redirects, CSS URLs, `srcset`, and nested resource
markup. No live user mail or settings and no external tracking service are
used:

```sh
WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 \
  tests/run_with_headless_weston.sh dbus-run-session -- \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
  indexed_remote_images_are_blocked_except_for_one_selected_message_load \
  -- --exact --nocapture --test-threads=1
```

A second loopback-only smoke opens Visual HTML in two simultaneously live
standalone message windows while the global override is enabled, disables it
through the real Settings dialog, and verifies that both existing WebViews are
immediately re-rendered with remote content blocked and no additional tracker
requests:

```sh
WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 \
  tests/run_with_headless_weston.sh dbus-run-session -- \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
  standalone_remote_images_are_revoked_when_settings_disable_them \
  -- --exact --nocapture --test-threads=1
```

Desktop `mailto` handling has cold-start and existing-instance GTK smokes. The
first verifies RFC 6068 fields remain visible while the startup search loads;
the second verifies D-Bus routing and the dirty-composer replacement prompt:

```sh
for test in \
  fixture_cold_mailto_launch_opens_prefilled_composer \
  fixture_existing_instance_mailto_request_confirms_dirty_replacement
do
  NOTM_REQUIRE_GTK_DISPLAY=1 \
    cargo test --locked -p notm-app --test desktop_ui_smoke "$test" -- \
      --exact --nocapture --test-threads=1
done
```

Vim-style message-list viewport scrolling has a GTK smoke. It routes `Ctrl+e`
and `Ctrl+y` through the main shortcut router, verifies movement in both
directions, and proves the selected message does not change:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
  fixture_ctrl_e_y_scroll_message_list_without_changing_selection \
  -- --exact --nocapture --test-threads=1
```

Remembered message views have a restart-backed GTK smoke. It verifies all
preference layers, drives the sender-default button and `V a` shortcut, checks
Message-ID precedence, and confirms that standalone windows resolve their own
selected message:

```sh
NOTM_REQUIRE_GTK_DISPLAY=1 \
  cargo test --locked -p notm-app --test desktop_ui_smoke \
  fixture_message_and_sender_views_persist_with_message_precedence \
  -- --exact --nocapture --test-threads=1
```

Use the fixture-only test-harness `send_key` command for application shortcut
checks that do not need compositor input; it calls the same ordered router as
the main window without focusing or presenting a window. The focused-text and
physical-key propagation regressions retain a self-contained headless Sway
check. It covers J/K message navigation, lowercase j/k message-body scrolling,
physical `Ctrl+e`/`Ctrl+y` message-list viewport scrolling without changing the
selection, the M current-message menu and its two-key actions, physical Shift+F
routing to link hints (with overlay behavior covered by the GTK smoke above),
and normal/insert-mode tag-editor safety. It also
drives the composer's real Vim Esc/Esc transition, completes `g d` while a
composer header field retains focus, deletes an indexed draft through physical
`D` and the real confirmation, and verifies physical `A`, `S`, and `x` actions
without allowing them to mutate the selected message. It requires
`dbus-run-session`, `sway`, `swaymsg`, and `wtype`:

```sh
cargo build --locked -p notm-app
python3 -B tests/ui_text_focus_smoke.py --binary target/debug/notm
```

Test-harness reports and screenshots are local validation artifacts under
`artifacts/`; they are ignored by git except for `artifacts/logs/.gitkeep`.
Keep one-off progress reports and completed planning notes out of the public
documentation tree.
