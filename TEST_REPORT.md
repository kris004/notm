# notm test report

## Slice 0 discovery

- OS: Linux developer-host 7.0.11-gentoo-dist x86_64.
- Rust: rustc 1.95.0, cargo 1.95.0.
- GTK4: pkg-config `gtk4` 4.20.4 present.
- libadwaita: pkg-config `libadwaita-1` 1.8.6 present.
- libnotmuch: `/usr/include/notmuch.h` and `/usr/lib64/libnotmuch.so` present; `pkg-config notmuch` is missing on this host, so build.rs attempts pkg-config first and then falls back to the detected header/library.
- bindgen: 0.72.1 CLI present; crate build uses bindgen.
- `gmi`: `/usr/bin/gmi` present.
- notmuch CLI: `/usr/bin/notmuch` present but is not used by app/test behavior.
- Send helper: `/home/user/.local/bin/aerc-gmail-send` found; it execs `gmi send -C /home/user/Mail/account.gmail --blocking`.
- Notmuch config: `/home/user/.config/notmuch/default/config` with database path `/home/user/Mail`, primary email `user@example.com`, excluded tags `deleted;spam`, maildir synchronize flags true.
- Crate inspection: `notmuch` 0.8.0 exists but targets feature eras up to v0_32; `notmuch-sys` 4.4.2 does not exactly match installed libnotmuch 5.7.0, so this project generates bindings from the installed header.

## Final quality gates run

### Format

`CARGO_HOME=$PWD/.cargo-home cargo fmt --all -- --check`

Result: passed.

### Clippy

`CARGO_HOME=$PWD/.cargo-home cargo clippy --workspace --all-targets -- -D warnings`

Result: passed.

### Tests

`CARGO_HOME=$PWD/.cargo-home cargo test --workspace`

Result: passed.

Coverage includes:

- native fixture database creation and indexing through libnotmuch,
- thread search,
- tag operations and undo through libnotmuch,
- MIME/HTML sanitization,
- attachment metadata and attachment byte extraction,
- RFC5322 fake send contract,
- RFC5322 attachment send contract,
- reply-all metadata and own-address exclusion,
- explicitly gated desktop/live tests.

### Fixture smoke

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- fixture-smoke`

Result: passed; 8 fixture inbox threads found; fake send captured an RFC5322 message under `artifacts/captured-send/`.

### Send transport probe

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- probe-send`

Result: passed.

Details:

- command exists: `/home/user/.local/bin/aerc-gmail-send`
- helper looks like Gmail/lieer send helper,
- auto mode uses stdin-RFC5322 and appends `-t` when no explicit args are configured,
- lieer repo exists: `/home/user/Mail/account.gmail`.

### Live read-only smoke

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- live-readonly-smoke`

Result: passed. Opened `/home/user/Mail` read-only via libnotmuch. Current observed revision: `405208`, UUID `00000000-0000-0000-0000-000000000000`. Default query `tag:inbox and not tag:deleted` returned 25 paged threads. No sync or mutation command was run.

GTK live screenshot from earlier in this run:

- `artifacts/screenshots/12_live_inbox_readonly.png`

## GTK fixture automation smoke

Launched actual desktop app:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- launch --fixture --automation --automation-socket /tmp/notm-fixture.sock --automation-token notm-test-token`

Automation drove: health, app_state, saved search, raw search, thread open, HTML rendering, tag archive, compose, fake send, reply, debug panel, screenshots.

Result: passed after fixing an initial GTK paned-layout bug.

Screenshots:

- `artifacts/screenshots/01_app_start.png`
- `artifacts/screenshots/02_fixture_inbox.png`
- `artifacts/screenshots/03_search_results.png`
- `artifacts/screenshots/04_thread_open.png`
- `artifacts/screenshots/05_message_rendering_plain_html.png`
- `artifacts/screenshots/06_tag_action_before.png`
- `artifacts/screenshots/07_tag_action_after.png`
- `artifacts/screenshots/08_compose.png`
- `artifacts/screenshots/09_fake_send_success.png`
- `artifacts/screenshots/10_reply_compose.png`
- `artifacts/screenshots/11_settings_debug.png`

## Final polish GTK automation smoke

Launched actual updated desktop app:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- launch --fixture --automation --automation-socket /tmp/notm-final.sock --automation-token notm-final-token`

Automation drove: health, app_state, address suggestions, recipient Tab autocomplete, compose attachment add, draft save, search for attachment message, open selected thread, select message, save selected attachment, command-palette debug toggle, screenshot, and draft clear. A second final UI smoke drove archive and undo through the actual GTK app after the last libnotmuch thaw-safety patch.

Result: passed. Output recorded at:

- `artifacts/reports/final-polish-automation.jsonl`
- `artifacts/reports/final-ui-tag-smoke.jsonl` (final post-wrapper-edit tag/undo UI smoke)

Artifacts:

- `artifacts/screenshots/15_finished_polish.png`
- `artifacts/screenshots/16_message_actions.png`
- `artifacts/attachments/note.txt`


## Message action and command palette GTK smoke

Launched actual updated desktop app with isolated fixture draft storage:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- launch --fixture --automation --automation-socket /tmp/notm-actions2.sock --automation-token notm-actions2-token`

Automation drove: initial fixture state with no restored personal draft recipients, command-palette command execution, attachment-message search/open, selected-message selection, copy message id, copy thread id, raw source view, rendered view, direct attachment save, open-attachment command availability, command palette open, raw-source command execution, and screenshot capture.

Result: passed. Redacted output recorded at:

- `artifacts/reports/message-actions-automation.jsonl`

Artifacts:

- `artifacts/screenshots/16_message_actions.png`
- `artifacts/attachments/note.txt`

## Live self-send gate

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- live-self-send`

This was run exactly once successfully earlier in this autonomous run and was not repeated during final polish.

Initial live send attempts:

- Sandbox run failed before sending because lieer could not create `.lock` in the read-only sandbox.
- Escalated run with plain stdin failed before sending with lieer error: recipients in sendmail args differed from headers; suggested missing `-t`.

Fix applied: auto mode detects the lieer helper and appends `-t` when no explicit args are configured.

Final live self-send:

- Subject: `notm live self-test 20260619T044204Z 5cf57808`.
- Transport accepted the message; exit status 0.
- The message appeared in Notmuch without forced sync.
- No `notmuch new`, `gmi sync`, `mbsync`, `offlineimap`, `lieer sync`, or receive command was run.

Reports:

- `artifacts/reports/live-self-send.txt` first sandbox pre-send failure.
- `artifacts/reports/live-self-send-escalated.txt` plain-stdin pre-send failure.
- `artifacts/reports/live-self-send-final.txt` accepted live send.

Screenshots:

- `artifacts/screenshots/13_live_self_send_result.png`
- `artifacts/screenshots/14_live_self_send_indexed.png`

## CLI/shell-out audit

- Production/test Notmuch behavior uses libnotmuch FFI; no production code shells out to the `notmuch` CLI.
- `std::process::Command` is used only for the configured external send transport and screenshot fallback tools.
- Sync commands are represented in config but not executed by default.

## Gap 1 live GTK UI send validation

Launched actual desktop app against the live Notmuch database:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- launch --automation --automation-socket /tmp/notm-live-ui-send.sock --automation-token notm-live-ui-send-token`

Automation sent two live messages through the real GTK composer/send path. Consistent subject prefix: `notm validation self-test`.

Subjects:

- `notm validation self-test 20260619T160138Z plain 2a1e4d48`
- `notm validation self-test 20260619T160138Z attachment 318a8151`

Result: both sends accepted by the external lieer helper with exit status 0. Both subjects appeared in Notmuch via app/libnotmuch search without forced sync. No receive/sync/notmuch-new command was run. Local draft cache was absent after send.

Artifacts:

- `artifacts/reports/gap1-live-ui-send-summary.json`
- `artifacts/reports/gap1-live-ui-send.jsonl` redacted trace
- `artifacts/screenshots/17_live_ui_send_plain.png`
- `artifacts/screenshots/17_live_ui_send_attachment.png`
- `artifacts/screenshots/18_live_ui_send_indexed.png`

## Gap 2 settings/custom saved-search/tag-editor gate

Launched actual fixture GTK app:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- launch --fixture --automation --automation-socket /tmp/notm-gap2b.sock --automation-token notm-gap2b-token`

Automation validated:

- editable settings persistence to fixture app config (`page_size = 42`, default query),
- custom saved-search persistence for `Gap2 HTML` -> `subject:"HTML message"`,
- selecting that custom saved search and seeing the expected HTML fixture thread,
- GUI custom tag entry path by adding and removing `notm-gap2`,
- settings dialog opening,
- screenshot capture.

Artifacts:

- `artifacts/reports/gap2-summary.json`
- `artifacts/reports/gap2-settings-saved-tags.jsonl`
- `artifacts/screenshots/19_settings_saved_search_tag_editor.png`

Quality gate after implementation passed: fmt check, clippy `-D warnings`, workspace tests, fixture smoke, live read-only smoke.

## Gap 3 drafts/address UI polish gate

Launched actual fixture GTK app:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- launch --fixture --automation --automation-socket /tmp/notm-gap3.sock --automation-token notm-gap3-token`

Automation validated:

- visible address suggestions for `ali`,
- selecting suggestion `alice@example.test` into the To field,
- saving two separate local named drafts,
- listing drafts,
- loading a selected draft,
- deleting a selected draft,
- screenshot capture.

Artifacts:

- `artifacts/reports/gap3-summary.json`
- `artifacts/reports/gap3-drafts-address.jsonl`
- `artifacts/screenshots/20_drafts_address_manager.png`

Quality gate after implementation passed: fmt check, clippy `-D warnings`, workspace tests, fixture smoke, live read-only smoke.

## Gap 4 performance hardening gate

Launched actual fixture GTK app:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- launch --fixture --automation --automation-socket /tmp/notm-gap4.sock --automation-token notm-gap4-token`

Automation validated:

- debounced search-entry update,
- rapid stale query replacement, with final query winning,
- repeat query served from query+revision cache,
- screenshot capture.

Artifacts:

- `artifacts/reports/gap4-summary.json`
- `artifacts/reports/gap4-search-performance.jsonl`
- `artifacts/screenshots/21_search_debounce_cache.png`

Quality gate after implementation passed: fmt check, clippy `-D warnings`, workspace tests, fixture smoke, live read-only smoke.

## Gap 5 thread/message indicators and viewer toggles gate

Launched actual fixture GTK app:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- launch --fixture --automation --automation-socket /tmp/notm-gap5.sock --automation-token notm-gap5-token`

Automation validated:

- attachment thread row detail cache reports `has_attachment = true`,
- attachment preview includes the fixture body preview,
- opened attachment thread renders attachment list with `note.txt`,
- opened three-message thread renders quoted text before collapse,
- quote-collapse toggle replaces quoted block with `[quoted text collapsed]`,
- full-header toggle displays raw headers including `Message-ID` and `Content-Type`,
- screenshot capture.

Artifacts:

- `artifacts/reports/gap5-summary.json`
- `artifacts/reports/gap5-thread-viewer.jsonl`
- `artifacts/screenshots/22_thread_indicators_viewer_toggles.png`

Quality gate after implementation passed: fmt check, clippy `-D warnings`, workspace tests, fixture smoke, live read-only smoke.

## Gap 6 forward-as-attachment and explicit sent/draft indexing gate

Launched actual fixture GTK app with isolated config enabling sent/draft Maildir indexing:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- --config artifacts/fixtures/gap6-config.toml launch --fixture --automation --automation-socket /tmp/notm-gap6.sock --automation-token notm-gap6-token`

Automation validated:

- fake send accepted a composed message,
- with `send.save_sent=true` and `send.index_sent_after_send=true`, the sent copy was saved under the fixture database Maildir and indexed through libnotmuch with tag `notm-gap6-sent`,
- with `[drafts].save_maildir=true` and `[drafts].index_after_save=true`, a draft copy was saved under the fixture database Maildir and indexed through libnotmuch with tag `notm-gap6-draft`,
- selected fixture message opened forward-as-attachment composer with an `.eml` attachment,
- screenshot capture.

Artifacts:

- `artifacts/fixtures/gap6-config.toml`
- `artifacts/reports/gap6-summary.json`
- `artifacts/reports/gap6-forward-sent-draft.jsonl`
- `artifacts/screenshots/23_forward_attachment_sent_draft_indexing.png`

Quality gate after implementation passed: fmt, clippy `-D warnings`, workspace tests, fixture smoke, live read-only smoke.

## Gap 7 final validation gate

Final command gate:

`CARGO_HOME=$PWD/.cargo-home cargo fmt --all -- --check`
`CARGO_HOME=$PWD/.cargo-home cargo clippy --workspace --all-targets -- -D warnings`
`CARGO_HOME=$PWD/.cargo-home cargo test --workspace`
`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- fixture-smoke`
`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- probe-send`
`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- live-readonly-smoke`

All passed. Output saved to `artifacts/reports/final-full-validation.txt`.

Final actual GTK fixture app launch:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- launch --fixture --automation --automation-socket /tmp/notm-final-gap7.sock --automation-token notm-final-gap7-token`

Automation validated health, fixture inbox search, thread open, message-view text, debug toggle, and screenshot capture.

Artifacts:

- `artifacts/reports/final-gap7-ui-smoke.jsonl`
- `artifacts/reports/final-gap7-ui-summary.json`
- `artifacts/screenshots/24_final_gap7_ui_smoke.png`

CLI/shell-out audit remains unchanged: no production or normal test behavior shells out to the `notmuch` CLI. `std::process::Command` is used for configured external send transport and screenshot fallback only. No receive/sync command ran by default.

## Final post-polish manual sync default gate

After adding the opt-in manual sync action, launched the actual GTK fixture app again:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- launch --fixture --automation --automation-socket /tmp/notm-final2.sock --automation-token notm-final2-token`

Automation validated health, fixture inbox search, thread open, `run_manual_sync` default no-op/disabled behavior, and screenshot capture. No sync command ran.

Artifacts:

- `artifacts/reports/final-ui-post-sync-polish.jsonl`
- `artifacts/reports/final-ui-post-sync-polish-summary.json`
- `artifacts/screenshots/25_final_ui_post_sync_polish.png`

## Post-final selectable attachment-list gate

Launched actual fixture GTK app:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- launch --fixture --automation --automation-socket /tmp/notm-attachlist.sock --automation-token notm-attachlist-token`

Automation validated:

- searching fixture attachment message,
- opening the thread,
- visible attachment list exposed `note.txt`,
- selected attachment row 0,
- saved selected attachment to `artifacts/attachments/postfinal/note.txt`,
- verified saved bytes equal fixture attachment body,
- screenshot capture.

Artifacts:

- `artifacts/reports/postfinal-attachment-list.jsonl`
- `artifacts/reports/postfinal-attachment-list-summary.json`
- `artifacts/screenshots/26_attachment_list_selection.png`

Quality gate passed: fmt, clippy `-D warnings`, workspace tests, fixture smoke, live read-only smoke.

## Post-final shortcuts overlay gate

Launched actual fixture GTK app:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- launch --fixture --automation --automation-socket /tmp/notm-shortcuts.sock --automation-token notm-shortcuts-token`

Automation validated `open_shortcuts` and screenshot capture.

Artifacts:

- `artifacts/reports/postfinal-shortcuts-overlay.jsonl`
- `artifacts/reports/postfinal-shortcuts-overlay-summary.json`
- `artifacts/screenshots/27_shortcuts_overlay.png`

Quality gate passed: fmt, clippy `-D warnings`, workspace tests, fixture smoke, live read-only smoke.

## Desktop launcher install gate

Commands run:

- `desktop-file-validate packaging/notm.desktop`
- `CARGO_HOME=$PWD/.cargo-home cargo build -p notm-app --release`
- `install -Dm755 target/release/notm /home/user/.local/bin/notm`
- `install -Dm644 packaging/notm.desktop /home/user/.local/share/applications/notm.desktop`
- `update-desktop-database /home/user/.local/share/applications`
- `desktop-file-validate /home/user/.local/share/applications/notm.desktop`
- `/home/user/.local/bin/notm --version`
- `gtk-launch notm` smoke launch; it started `/home/user/.local/bin/notm launch` and the test process was closed.

Installed paths:

- Binary: `/home/user/.local/bin/notm`
- Desktop entry: `/home/user/.local/share/applications/notm.desktop`
- Source desktop entry: `packaging/notm.desktop`

## Large-inbox slice 1 paging gate

Launched actual fixture GTK app with `artifacts/fixtures/large-inbox-paging-config.toml` (`ui.page_size = 3`):

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- --config artifacts/fixtures/large-inbox-paging-config.toml launch --fixture --automation --automation-socket /tmp/notm-large-page.sock --automation-token notm-large-page-token`

Automation validated:

- `run_search tag:inbox` loaded 3 of 8 threads,
- `thread_page_info` reported `page_size=3`, `loaded=3`, `total=8`, `can_load_more=true`,
- `load_more_threads` appended to 6 of 8 threads,
- screenshot capture.

Artifacts:

- `artifacts/fixtures/large-inbox-paging-config.toml`
- `artifacts/reports/large-inbox-paging.jsonl`
- `artifacts/reports/large-inbox-paging-summary.json`
- `artifacts/screenshots/28_large_inbox_paging_load_more.png`

Quality gate passed: fmt, clippy `-D warnings`, workspace tests, fixture smoke, live read-only smoke.

## Visual HTML WebKitGTK gate

Verified WebKitGTK 6 availability:

- `pkg-config --modversion webkitgtk-6.0` -> `2.52.3`
- `pkg-config --modversion javascriptcoregtk-6.0` -> `2.52.3`
- `pkg-config --modversion gtk4` -> `4.20.4`

Implemented the visual HTML slice with `webkit6 = 0.6.1` and gtk4-rs `0.11`.

Launched actual fixture GTK app:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- --config artifacts/fixtures/large-inbox-paging-config.toml launch --fixture --automation --automation-socket /tmp/notm-html-visual.sock --automation-token notm-html-visual-token`

Automation validated:

- `run_search subject:HTML` found the fixture `HTML message`,
- opened the thread through the real GTK app,
- `html_view_state` before rendering reported `has_html=true`, `html_visible=false`, `visible_child=text`,
- `show_visual_html` loaded sanitized HTML into the WebKitGTK view,
- `html_view_state` after rendering reported `has_html=true`, `html_visible=true`, `visible_child=html`, `remote_images_allowed=false`,
- screenshot capture,
- after removing a deprecated WebKit setting call, re-launched the actual GTK/WebKit app and repeated the same automation path successfully with no WebKit hyperlink-auditing warning.

Artifacts:

- `artifacts/reports/webkit-html-visual.jsonl`
- `artifacts/reports/webkit-html-visual-rerun.jsonl`
- `artifacts/reports/webkit-html-visual-summary.json`
- `artifacts/screenshots/29_webkit_html_visual.png`

Quality gate after implementation passed:

- `CARGO_HOME=$PWD/.cargo-home cargo fmt --all -- --check`
- `CARGO_HOME=$PWD/.cargo-home cargo clippy --workspace --all-targets -- -D warnings`
- `CARGO_HOME=$PWD/.cargo-home cargo test --workspace`
- `CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- fixture-smoke`
- `CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- probe-send`
- `CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- live-readonly-smoke`

Send transport probe after WebKit refresh passed: command `/home/user/.local/bin/aerc-gmail-send` exists, the lieer repository exists at `/home/user/Mail/account.gmail`, and auto mode uses stdin-RFC5322 plus `-t` for the Gmail/lieer helper.

Install refresh passed:

- `CARGO_HOME=$PWD/.cargo-home cargo build --release -p notm-app`
- `install -m 0755 target/release/notm /home/user/.local/bin/notm`
- `/home/user/.local/bin/notm --help`

No receive/sync command ran. No production or normal-test code shells out to the `notmuch` CLI.

Installed binary smoke after WebKit refresh:

- `/home/user/.local/bin/notm fixture-smoke` -> passed with 8 fixture inbox threads and fake send capture.

## Image policy, Enter shortcut, and scroll-bottom load-more gate

Launched actual fixture GTK/WebKit app:

`CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- --config artifacts/fixtures/large-inbox-paging-config.toml launch --fixture --automation --automation-socket /tmp/notm-image-scroll.sock --automation-token notm-image-scroll-token`

Automation validated:

- `show_visual_html` on the fixture HTML message reports `image_loading_allowed=false`, `policy_allows_images=false`, and `sender_trusted=false`,
- `load_images_once` reports `image_loading_allowed=true` while `sender_trusted=false`,
- a subsequent normal `show_visual_html` blocks images again, proving the one-shot action is not persistent,
- `trust_sender_images` stores `html@example.test`, reports `sender_trusted=true`, and enables images,
- a subsequent normal `show_visual_html` reports `policy_allows_images=true` and `image_loading_allowed=true` for the trusted sender,
- `run_search tag:inbox` with `ui.page_size=3` loaded 3 of 8 threads,
- `scroll_thread_list_to_bottom` auto-loaded the next page to 6 of 8 threads,
- screenshot capture.

Artifacts:

- `artifacts/reports/image-policy-scroll.jsonl`
- `artifacts/reports/image-policy-scroll-summary.json`
- `artifacts/screenshots/30_html_image_policy_controls.png`
- `artifacts/screenshots/31_auto_load_more_scroll.png`

Enter-key fix: the global key controller now uses GTK capture phase. This prevents a focused toolbar `Compose` button from receiving Enter before notm handles it; Enter still proceeds normally in text fields and Ctrl+Enter still sends from the composer.

Quality gate after implementation passed:

- `CARGO_HOME=$PWD/.cargo-home cargo fmt --all -- --check`
- `CARGO_HOME=$PWD/.cargo-home cargo clippy --workspace --all-targets -- -D warnings`
- `CARGO_HOME=$PWD/.cargo-home cargo test --workspace`
- `CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- fixture-smoke`
- `CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- probe-send`
- `CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- live-readonly-smoke`
- `CARGO_HOME=$PWD/.cargo-home cargo build --release -p notm-app`
- `install -m 0755 target/release/notm /home/user/.local/bin/notm`
- `/home/user/.local/bin/notm fixture-smoke`

No receive/sync command ran. No live email was sent. No production or normal-test code shells out to the `notmuch` CLI.
