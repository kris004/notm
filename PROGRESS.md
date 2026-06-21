# notm progress

## Completed in this autonomous run

- Created the full requested Cargo workspace and repository/documentation layout.
- Implemented native bindgen/libnotmuch FFI with safe RAII wrappers for database open/create/load, query threads/messages, tags, filenames, config values, revision/UUID, fixture indexing, and tag mutation with freeze/thaw and optional Maildir flag sync.
- Implemented Rust MIME parsing, sanitized HTML-to-text fallback, attachment detection and byte extraction, RFC5322 composition with attachments, reply/reply-all, inline forward, fake send transport, and external command transport with stdin/file/template/auto modes.
- Implemented a GTK4 native desktop app with sidebar saved searches, search/thread list, thread/message view, composer, local draft recovery, address suggestions with Tab completion, tag actions, undo tag, settings/debug dialogs, command palette, local Unix socket automation, interactive command palette, direct raw-source/copy/save-attachment message actions, and screenshot fallback.
- Implemented fixture Maildir + native Notmuch database creation without `notmuch` CLI.
- Ran fixture automation through the actual GTK app and captured screenshots 01-11.
- Ran live read-only smoke against `/home/user/Mail` and captured screenshot 12.
- Probed the real `aerc-gmail-send` helper, learned that lieer needs `-t` for header recipients, updated auto mode, and completed exactly one live self-send that appeared in Notmuch without forced sync; captured screenshots 13-14.
- Continued polish after the self-send without sending another live email: wired automation aliases, address-suggestion commands, compose attachment add, isolated fixture draft storage, draft save/clear/load, selected-message selection, command-palette execution, raw/rendered source toggles, copy message/thread id, and direct attachment save. Captured screenshots 15-16.

## Current known limitations

- GTK UI is usable but visually basic; not yet libadwaita-polished.
- Address autocomplete is implemented as cached suggestions plus Tab completion/automation, not full GTK dropdown chips.
- Draft persistence includes local JSON recovery plus a multi-draft local manager; optional Maildir draft save/indexing is implemented only when explicitly configured. Fixture launches use isolated temp draft storage by default.
- Thread rows show unread/flagged/attachment/encrypted/signed indicators and safe body previews. Attachments are detected, rendered, composable, extractable, and saveable from the message action row; opening via the desktop default app is implemented from the message action row.
- Keyboard shortcuts cover common one-key actions, Ctrl+K/Ctrl+Enter, and `g i`/`g u`/`g f`/`g s`/`g a`; a polished shortcuts overlay remains basic.
- Automation covers the main daily-driver flows and extra aliases, but it is intentionally local/debug tooling rather than a public API.
- `pkg-config notmuch` is missing on this host, so build.rs falls back to `/usr/include/notmuch.h` and `-lnotmuch` after trying pkg-config.

## Remaining-work tracking

The remaining gaps are tracked in `WORK_QUEUE.md` and will be closed one at a time with quality gates after each gap.

## Gap 1 complete: live GTK UI send validation

Sent two bounded live self-test messages through the actual GTK composer automation path using subject prefix `notm validation self-test`: one plain and one with an attachment. Both external lieer sends exited 0 and both appeared in Notmuch without forced sync. Draft cache was absent after send. Reports: `artifacts/reports/gap1-live-ui-send-summary.json`, redacted command trace `artifacts/reports/gap1-live-ui-send.jsonl`. Screenshots: `artifacts/screenshots/17_live_ui_send_plain.png`, `artifacts/screenshots/17_live_ui_send_attachment.png`, `artifacts/screenshots/18_live_ui_send_indexed.png`.

## Gap 2 complete: settings, custom saved searches, tag editor

Implemented editable settings persistence to the app config, custom saved-search editor with persistence, saved-search automation, and visible custom tag add/remove controls. Fixture launches write to an isolated temp app config. Validated by GTK automation: saved page_size/default_query, saved and selected `Gap2 HTML`, added and removed custom tag `notm-gap2`, opened settings, and captured `artifacts/screenshots/19_settings_saved_search_tag_editor.png`.

## Gap 3 complete: drafts and address UI polish

Implemented a local multi-draft manager in the composer: draft list, save as separate timestamped draft files, load selected draft, and delete selected draft. Fixture launches use isolated temp draft directories. Implemented visible address suggestion list with row activation plus automation selection, while preserving Tab completion. Validated by GTK automation: completed `ali` to `alice@example.test`, saved two drafts, listed drafts, loaded selected draft, deleted one draft, and captured `artifacts/screenshots/20_drafts_address_manager.png`.

## Gap 4 complete: search performance hardening

Implemented debounced search-entry changes with a background search worker, stale generation checks to discard old delayed results, and a process cache keyed by database path + database UUID/revision + page size + excluded tags + query. Synchronous search paths also use the cache. Validated by GTK automation with rapid query changes and a repeat query returning `from cache`; captured `artifacts/screenshots/21_search_debounce_cache.png`.

## Gap 5 complete: thread indicators and viewer toggles

Implemented cached per-thread UI details keyed by database path + Notmuch revision + thread id. Thread rows now show unread/flagged/attachment/encrypted/signed indicators and safe body previews without loading all mail. Added message-view actions and automation for full headers, raw source, rendered view, quote collapse, message-view text inspection, and thread detail inspection. Validated against the actual fixture GTK app: attachment indicator/preview, attachment rendering, quote collapse on a three-message thread, full header display, and screenshot capture. Screenshot: `artifacts/screenshots/22_thread_indicators_viewer_toggles.png`.

## Gap 6 complete: forward-as-attachment and explicit sent/draft indexing

Implemented forward-as-attachment for the selected message using a `message/rfc822` `.eml` attachment cached into the local compose attachment cache. Added explicit, opt-in sent persistence (`send.save_sent`, `send.sent_maildir`, `send.sent_tags`, `send.index_sent_after_send`) and draft Maildir/indexing persistence (`[drafts] save_maildir`, `maildir`, `tags`, `index_after_save`). Defaults remain disabled, so no real mail files are written/indexed unless configured. Validated against an isolated fixture GTK app with fake send: sent message saved/indexed with `notm-gap6-sent`, draft saved/indexed with `notm-gap6-draft`, and forward-as-attachment composer produced an `.eml` attachment. Screenshot: `artifacts/screenshots/23_forward_attachment_sent_draft_indexing.png`.

## Gap 7 complete: final validation and documentation

Updated README, SECURITY, and docs to reflect the finished feature set and opt-in indexing behavior. Re-ran final validation: fmt check, clippy `-D warnings`, workspace tests, fixture smoke, send probe, live read-only smoke. Launched the actual final GTK fixture app, drove it with automation, opened a thread, toggled debug panel, and captured `artifacts/screenshots/24_final_gap7_ui_smoke.png`. Final validation report: `artifacts/reports/final-full-validation.txt`.

## Final post-polish sync default check

Added a manual sync action that is hidden/disabled by default and only runs explicitly configured commands when sync is enabled. Re-launched the final GTK fixture app after this polish, verified `run_manual_sync` is a no-op with `last_operation = manual sync disabled` under defaults, and captured `artifacts/screenshots/25_final_ui_post_sync_polish.png`.

## Post-final improvement: selectable thread attachment list

Added a visible attachment list below the message action row. Opening a thread now populates all attachments in the visible thread with message index, filename, content type, and size. The Save/Open attachment buttons use the selected attachment row, and automation now exposes `attachment_list_items` and `select_attachment_by_index`. Validated against the actual GTK fixture app by opening the fixture attachment message, selecting `note.txt`, saving it to `artifacts/attachments/postfinal/note.txt`, verifying bytes, and capturing `artifacts/screenshots/26_attachment_list_selection.png`.

## Post-final improvement: dedicated shortcuts overlay

Added a native shortcuts overlay dialog and changed `?` to open it directly instead of reusing the command palette. Automation now exposes `open_shortcuts`/`show_shortcuts`, and the command palette can run `shortcuts`. Validated against the actual GTK fixture app and captured `artifacts/screenshots/27_shortcuts_overlay.png`.

## Desktop launcher installed

Added `packaging/notm.desktop`, built the release binary, installed it to `/home/user/.local/bin/notm`, installed the desktop entry to `/home/user/.local/share/applications/notm.desktop`, refreshed the user desktop database, validated the desktop file, and smoke-launched it once with `gtk-launch notm`. The smoke launch created process `notm launch` and that test process was closed afterward.

## Large-inbox slice 1: paged search/load more

Implemented real query pagination for thread results. Search now records loaded count, total count, page size, and can-load-more state. Added a Load more button and automation commands `load_more_threads` and `thread_page_info`. Search cache keys now include page offset. Validated against fixture GTK app with `page_size = 3`: initial `tag:inbox` load returned 3 of 8 threads, `load_more_threads` appended to 6 of 8 without replacing results, and screenshot `artifacts/screenshots/28_large_inbox_paging_load_more.png` was captured.

## Visual HTML slice complete: sanitized WebKitGTK renderer

Implemented a real visual HTML message view using WebKitGTK 6 while keeping the safe rendered text fallback. Added a `Visual HTML` message action, a GTK stack that switches between text and WebKit views, WebKit settings that disable JavaScript, JavaScript markup, file/universal file access, developer extras, and remote image loading by default, plus in-app navigation blocking with status-bar target reporting. Automation now exposes `show_visual_html`/`show_html_visual` and `html_view_state`. Validated against the actual GTK fixture app on the fixture HTML message: before visual rendering the stack was `text` with `has_html=true`; after `show_visual_html`, automation reported `html_visible=true`, `has_html=true`, `remote_images_allowed=false`, and screenshot `artifacts/screenshots/29_webkit_html_visual.png` was captured. Rebuilt and reinstalled `/home/user/.local/bin/notm` with the WebKit renderer.

## Post-final improvement: image controls, Enter fix, and scroll-bottom paging

Added remote image controls to the WebKit HTML view. Normal Visual HTML still blocks images unless global `ui.remote_images` or the selected sender is trusted. `Load images once` allows remote images only for the current render. `Trust sender images` persists the selected sender to `[ui].trusted_image_senders` and future normal Visual HTML renders allow images for that sender. Added automation commands for `load_images_once`, `trust_sender_images`, `trusted_image_senders`, and enriched `html_view_state`. Fixed the Enter shortcut by moving the global key controller to capture phase so Enter opens the selected thread before a focused toolbar button can activate Compose. Added scroll-bottom auto-load-more on the thread list and automation command `scroll_thread_list_to_bottom`. Validated against the actual GTK/WebKit fixture app and captured `artifacts/screenshots/30_html_image_policy_controls.png` and `artifacts/screenshots/31_auto_load_more_scroll.png`. Rebuilt and reinstalled `/home/user/.local/bin/notm`.
