# Developer test harness

This is not mail automation, filtering, or rules support. It is a local
UI-driving test harness for developers and automated checks working on `notm`.

Normal users do not need the test harness enabled to use `notm`; the public
README intentionally stays focused on the desktop mail experience.

The harness uses a local Unix-domain socket. It is disabled by default and should
be enabled only for a controlled local run. Requests are JSON lines containing
`token`, `command`, and optional `args`. Commands are dispatched into the GTK
main loop and operate on the real app model/widgets.

## Launch with the test harness

```sh
cargo run --locked -p notm-app -- launch \
  --test-harness \
  --test-harness-socket /tmp/notm.sock \
  --test-harness-token dev-token
```

Against disposable fixture data:

```sh
cargo run --locked -p notm-app -- launch \
  --fixture \
  --test-harness \
  --test-harness-socket /tmp/notm.sock \
  --test-harness-token dev-token
```

Example request:

```json
{"token":"dev-token","command":"run_search","args":{"query":"tag:inbox"}}
```

`run_search` schedules work and returns immediately with `scheduled: true` and
the new generation. `select_saved_search` and `load_more_threads` use the same
background-search state. Poll `search_status` until `loading` is false before
inspecting final rows; a non-null `error` means the current generation failed.
Tag commands likewise return `pending: true` after scheduling. Poll
`tag_status` until `in_progress` is false for writer completion. A tag result
may then start an authoritative reconciliation search; if `search_status`
reports `loading: true`, poll it until `loading` is false before inspecting
final rows, current filenames, undo history, or path-based message actions.
Fixture harnesses may add `test_delay_ms` (up to 5000) to `run_search` or
`tag_selected` when testing responsiveness. A disposable non-fixture tag-race
harness may use the delay only with `automation.allow_live_tag_test = true`;
other non-fixture runs reject it.
If `tag_status.paths_uncertain` is true, retained message and draft paths are
intentionally disabled and the process must be restarted before driving any
path, tag, send, or sync action. A reported partial result with known paths
instead remains blocked only through its automatic reconciliation search.

Thread/message and message-derived composer preparation plus recovery-draft
persistence have equivalent latency controls. `set_fixture_thread_delay`,
`set_fixture_composer_preparation_delay`, `set_fixture_draft_delay`, and
`set_fixture_attachment_delay` inject at most 5000 milliseconds of worker-side
I/O delay. `set_fixture_thread_delay` is also available to a disposable
non-fixture tag-race harness when `automation.allow_live_tag_test = true`, so a
real Maildir rename can overlap cancellable preparation; the other latency and
failure-injection controls remain fixture-only. `fail_next_draft_write` and
`fail_next_attachment_write` inject one recovery-draft or attachment write
failure, respectively. Status commands remain read-only and available in any
test-harness run.
`thread_load_status`, `composer_preparation_status`, `draft_autosave_status`,
`draft_io_status`, `recovery_load_status`, and `attachment_io_status` report
the current generation, activity, and last completion/error. The attachment
status also reports asynchronous composer attachment caching. Poll those
status commands rather than waiting inside GTK.
The `health` response includes those activities plus a monotonically increasing
`gtk_heartbeat`, so a responsiveness smoke can prove that timers and harness
input continue to run while slow work is outstanding.
`thread_load_status` additionally exposes prepared message/attachment counts,
estimated retained bytes, and active/peak payload-preparation workers; the peak
is bounded to one and queued stale generations are coalesced. Fixture tests may
set `NOTM_FIXTURE_TEST_LARGE_ATTACHMENT_BYTES` (capped at 8 MiB) to enlarge the
first attachment-heavy payload without changing ordinary fixture runs.

Startup recovery has a pre-harness fixture seam because a command cannot delay
work that begins while the window is being built. Set
`NOTM_FIXTURE_TEST_STARTUP_RECOVERY_DELAY_MS` on a fixture test-harness process
to delay its recovery worker by at most 5000 milliseconds. The variable is
ignored unless both fixture mode and the test harness are enabled. It never
enables a live-mail or send side effect.

`load_draft` also schedules this bounded recovery reader and returns
`pending: true` with its generation; it does not read or parse the file inside
the harness callback. Poll `recovery_load_status` for `busy: false`. If the
composer changes before completion, the outcome is `superseded` and the newer
composer/recovery state is retained.

## Safety boundaries

- Keep the harness socket local-only. Do not expose it outside the user session.
- The default is a mode-0600 per-process socket under an absolute
  `$XDG_RUNTIME_DIR`, with `/tmp` as a fallback. Existing regular files,
  symlinks, and active sockets are not replaced.
- Always use a token for non-fixture runs.
- Prefer fixture runs before live mailbox runs.
- Fixture mode never runs configured receive/database-update commands or an
  external send helper. It captures sends locally and applies tags only to its
  disposable database.
- Non-fixture `compose_send` requests require
  `automation.allow_live_send_test = true`. Non-fixture tag mutations require
  `automation.allow_live_tag_test = true`. These gates also apply to actions
  reached through `run_command`; both default to false.
- Non-fixture receive/sync commands still use the explicit `[sync]` enablement
  and command settings. Run them only when the user intentionally requested the
  configured path.
- Draft save/delete operations use configured local paths. Attachment Save uses
  the GTK chooser and Attachment Open uses an application-owned private
  temporary directory; both are outside the live send/tag gates. The harness's
  explicit attachment `dir` bypass writes to that directory and should be used
  only for intentional storage-level tests.
- Screenshot and report artifacts are local validation outputs and are ignored by
  git except for `artifacts/logs/.gitkeep`.

## Command groups

Implemented test-harness commands include:

- health/state: `health`, `app_state`, `search_status`, `tag_status`,
  `thread_load_status`, `composer_preparation_status`,
  `draft_autosave_status`, `recovery_load_status`, `get_logs`, `screenshot`
- search/navigation: `focus_search`, `set_search_query`, `run_search`,
  `load_more_threads`, `scroll_thread_list_to_bottom`, `thread_page_info`,
  `thread_selection_view_state`, `thread_row_layout`, `thread_list_rows`,
  `select_saved_search`, `save_current_search`, `select_thread_by_index`,
  `select_relative_thread`, `select_thread_edge`, `open_selected_thread`,
  `select_message_by_index`, `select_relative_message`, `thread_ui_details`,
  `toggle_multi_select_thread`, `clear_multi_selection`
- tags: `archive_selected`, `mark_read_selected`, `mark_unread_selected`,
  `flag_selected`, `unflag_selected`, `trash_selected`, `spam_selected`,
  `tag_selected`, `add_tag_selected`, `remove_tag_selected`, `undo_last_tag`,
  `undo_tag_actions`, `message_tag_state`, `set_message_tag_entry`,
  `click_message_tag_action`
- compose/send: `open_compose`, `compose_set_from`, `compose_set_to`,
  `compose_set_cc`, `compose_set_bcc`, `compose_set_subject`,
  `compose_set_body`, `compose_add_attachment`, `compose_send`
- replies/forwards: `reply_selected`, `reply_all_selected`, `forward_selected`,
  `forward_as_attachment_selected` (these return `pending: true` with a
  generation while message-derived fields are prepared; poll
  `composer_preparation_status`)
- address/drafts/attachments: `get_address_suggestions`,
  `select_address_suggestion_by_index`, `autocomplete_recipient`,
  `attachment_list_items`, `select_attachment_by_index`, `save_draft`,
  `list_drafts`, `select_draft_by_index`, `load_selected_draft`,
  `delete_selected_draft`, `delete_active_draft`, `delete_local_draft`,
  `load_draft`, `clear_draft`, `draft_list_state`, `draft_io_status`,
  `refresh_named_drafts`, `set_fixture_draft_delay`, `fail_next_draft_write`,
  `activate_draft_by_index`, `click_delete_selected_draft`,
  `pending_confirmation`, `respond_confirmation`,
  `save_selected_attachment`, `save_attachment`, `open_selected_attachment`,
  `open_attachment`, `attachment_test_state`, `attachment_io_status`,
  `set_fixture_attachment_delay`, `fail_next_attachment_write`,
  `respond_attachment_save`
- message actions: `show_raw_source`, `open_raw_source`, `show_full_headers`,
  `full_headers`, `show_text_thread`, `show_rendered_thread`,
  `toggle_text_visual`, `show_visual_html`, `show_html_visual`, `image_policy`,
  `image_policy_menu`, `load_images_once`, `trust_sender_images`,
  `untrust_sender_images`, `trusted_image_senders`, `html_view_state`, `html_scroll_state`,
  `scroll_html_view_lines`,
  `standalone_message_windows`, `standalone_show_visual_html`,
  `standalone_image_policy`, `standalone_scroll_html_lines`,
  `view_preference_state`, `click_sender_view_preference`,
  `start_link_hints`, `link_hint_state`, `input_link_hint`,
  `cancel_link_hints`, `toggle_quote_collapse`, `message_view_text`,
  `copy_message_id`, `copy_thread_id`
- standalone windows: `standalone_message_windows`,
  `close_standalone_message_windows`, `standalone_select_message`,
  `standalone_respond`
- UI/debug: `send_key`, `open_command_palette`, `command_completion`,
  `open_shortcuts`, `show_shortcuts`, `help_search`, `run_command`,
  `run_manual_sync`, `open_settings`, `settings_test_state`,
  `respond_settings`, `save_settings`, `resize_window`, `pane_visibility`,
  `set_pane_visibility`, `layout_state`,
  `set_fixture_thread_delay`, `set_fixture_composer_preparation_delay`,
  `set_layout`, `toggle_layout`, `toggle_debug_panel`, `close_main_window`,
  custom saved-search commands, and custom tag-editor commands

`focus_search` and `focus_compose_field` move GTK focus without forcing Insert
mode. Tests that exercise keyboard editing should send the same `/`, `i`, or
Enter transition that a user would use instead of relying on the harness to
change modes implicitly.

Fixture harnesses can route an application shortcut directly through the same
ordered key router used by the main window with `send_key`. Pass a GDK key name
and optional `shift`, `control` (or `ctrl`), `alt`, and `super` modifiers, for
example `{"key":"J","modifiers":["shift"]}`. The response reports whether the
application handled the key. This does not synthesize a compositor event or
forward an unhandled key into a focused text widget; use it for notm shortcuts,
and reserve an isolated compositor input tool for GTK text-entry propagation
checks. Arbitrary shortcut routing is rejected outside fixture mode because a
shortcut can send mail or mutate tags.

`view_preference_state` reports the selected, resolved, and active message
views plus both persisted preference maps and the rendered sender-button state.
`click_sender_view_preference` emits the real View-menu button click. Both are
fixture-only UI-test controls; normal view selection uses the same persistence
path without requiring the harness.

`standalone_message_windows` includes each window's HTML lifecycle generation,
readiness, pending work, scroll metrics, and error plus the four-window cache
limit. `standalone_show_visual_html` and `standalone_scroll_html_lines` are
fixture-only controls for rapid replacement and event-driven scroll checks.

`click_message_tag_action` drives the real current-message menu buttons and
accepts `action` set to `archive`, `read`, `flag`, `trash`, `spam`, or `custom`.
The custom action also accepts `tag`. These operations target the selected
message ID only; thread selection and multi-selection are ignored. Like the
thread tag commands, they require fixture mode or
`automation.allow_live_tag_test=true`.

`load_images_once` reloads Visual HTML with remote images enabled only for the
current message view. It creates no durable sender permission and resets when
the test navigates away or restarts the application.

`image_policy` toggles the checked **Always load from this sender** item in the
fixed **Images** menu. `trust_sender_images` enables the same exact normalized
`From:`-mailbox exception idempotently, while `untrust_sender_images` revokes
it. `trusted_image_senders` reads the current list. Persistent actions write app
configuration, so direct and `run_command` harness access is fixture-only. A
failed write is reported and leaves runtime policy and rendered views
unchanged. Use `load_images_once` for an unrestricted transient check.
`image_policy_menu` opens or closes the real popover according to its `visible`
Boolean and is fixture-only.
`standalone_image_policy` opens the real standalone-window popover, activates
the corresponding control, closes the popover, and returns its visibility in
the window snapshot. Set `action` to `load_once`, `toggle_sender`, `sender_on`,
or `sender_off`; the command is also fixture-only and reports a persistence
failure instead of reporting a successful toggle.

`html_view_state` and standalone snapshots distinguish `blocked`,
`message_once`, `sender`, and `all_messages`. They expose the selected exact
image sender, whether its exception is active, the sender-control label and
sensitivity, and the visible spoofing warning. The warning is part of the
contract: `notm` does not authenticate raw `From:` or trust message-supplied
authentication headers, so a forged message claiming an allowed address
inherits the permission.

In fixture mode, `draft_list_state` reports whether the rendered Saved drafts
section, explicit empty state, bounded scroller, rows, and per-selection Delete
button are mapped. It also reports the selected row, compose fields, active
draft, and fixture persistence paths. `activate_draft_by_index` selects a row
and emits the real list activation signal; `click_delete_selected_draft` emits
the real Delete button click and reports whether its selected file was removed.
These three UI-test controls are rejected outside fixture mode. The older
`select_draft_by_index`, `load_selected_draft`, and `delete_selected_draft`
commands remain compatibility controls; load and delete still use the same
confirmation policy as their rendered UI routes.

Named-draft migration, bounded directory scanning, file reads, JSON parsing,
explicit save/index/replacement, and deletion run on workers. The rendered list
uses the last completed snapshot (at most 256 drafts, 2 MiB per file, and
32 MiB total), so a failed refresh leaves the last good list visible.
The fixture-only `refresh_named_drafts` command schedules that refresh and
returns its generation; poll `draft_io_status.list_busy` until it is false.
Its optional `migrate_legacy: true` path exercises the same bounded migration.
`set_fixture_draft_delay` also delays these explicit draft operations.

### Confirmation dialog seam

Destructive actions and actions that replace the composer can defer instead of
running immediately. This includes draft discard and permanent deletion, dirty
composer replacement by New/reply/forward/draft-load actions, and other routes
such as window close. A command that starts this flow
reports `pending_confirmation: true` where applicable; use
`pending_confirmation` as the authoritative state check. Sending an active
saved draft is also a typed `send_composer` confirmation because transport
acceptance will permanently delete that draft. Unsaved sends still start
directly. Accepted cleanup retains the existing draft-identity and
composer-generation checks.

`pending_confirmation` reports either `pending: null` or the captured action's
`id`, typed `kind`, dialog `title`, confirmation-button label, and current GTK
visibility. Its response also includes `last_completion` plus the current
compose fields, active draft, recovery and named-draft paths, visible status,
and last operation/error so a smoke can compare state across a response. Only
one action can be pending at a time.

Drive the real modal dialog with `respond_confirmation`. Set `response` to
`accept` or `reject`; optional `id` defaults to the current pending action and
guards against responding to a different dialog. For example:

```json
{"token":"dev-token","command":"respond_confirmation","args":{"id":1,"response":"reject"}}
```

The command emits the real GTK dialog response rather than calling the action
directly. Reject cancels without executing the captured replacement or
destructive action. Accept first revalidates current send/sync eligibility and
then executes that captured action once; the returned `last_completion` reports
its `accepted` and `succeeded` state. While a confirmation is pending, harness
mutations are rejected without changing its ID, dialog, compose state, recovery
bytes, or persisted drafts; read-only queries remain available.

Both confirmation commands are normally fixture-only. A non-fixture harness
with `automation.allow_live_send_test=true` may use them only while the pending
action is exactly `send_composer` or `close_main_window`; this narrow gate lets
disposable custom-transport smokes drive the real saved-draft Send and
close-flush modals without exposing draft deletion, composer replacement, or
other live confirmations.

`save_selected_attachment` and its `save_attachment` compatibility spelling
open the same GTK save chooser as the normal UI when `dir` is omitted. The
fixture-only `attachment_test_state` reports the pending chooser ID, sanitized
suggested name, visible status, private Open directory, and fake-opener calls.
Respond deterministically with
`respond_attachment_save {"id": ID, "response": "accept", "path":
"/full/target"}` or with `"response": "cancel"`. Accept treats that complete
target as authoritative and creates a numbered sibling rather than replacing
an existing file; cancel is a successful no-op. Supplying an explicit `dir` to
`save_selected_attachment` bypasses only the chooser. Both paths return
`pending: true` with a generation and request ID, perform collision-safe writes
on a worker, and complete even if the visible attachment selection changes.
Poll `attachment_io_status` for `busy: false` and inspect `last_completion`;
stale completions never replace newer UI status.

Attachment Open likewise prepares a safely named file beneath a private,
mode-0700 application directory on a worker and launches it only after the
current completion returns to GTK. Fixture mode records the path in a fake
opener instead of starting an external application. The directory and its files
remain available while the app runs and are removed when the process exits
normally.

After `open_settings`, the fixture-only `settings_test_state` reports the real
dialog ID and visible controls, the requested theme and live resolved
`theme_bg_color`/luminance plus raw GTK properties, the configured preview
limit, the send-timeout entry and configured launch value, and the actual
rendered preview label's line limit, visibility, and text. `respond_settings`
drives that same GTK dialog's response signal. It accepts optional `id`,
`theme`, `thread_preview_lines`, `show_thread_preview`, and
`send_timeout_seconds` arguments plus `response` set to `apply`, `save`, or
`close`, for example:

```json
{"token":"dev-token","command":"respond_settings","args":{"response":"apply","theme":"dark","thread_preview_lines":3,"show_thread_preview":true,"send_timeout_seconds":120}}
```

This is a fixture-only UI-test seam, not an alternate settings API. Invalid
theme, preview, or send-timeout values leave the dialog open and do not update
runtime state or the config file. Send timeouts use the same inclusive
1..=946080000-second range as normal configuration. `apply` changes the running
window without writing; `save` writes and then applies. Send changes require
relaunch. The older `save_settings` command persists only its basic direct-test
fields and does not exercise the dialog.

`run_manual_sync` returns with `pending: true` after the configured commands
have started in the background. Poll `app_state.state.sync_in_progress` until
it becomes false before asserting the command result or refreshed mail state;
the flag remains true until the post-sync search refresh also finishes.
Non-fixture harness checks may pass `test_refresh_delay_ms` (maximum 5000) to
keep that refresh pending while they exercise responsiveness and cancellation
behavior; normal UI and command-palette syncs never add this delay.

`compose_send` normally returns with `pending: true` after the composed snapshot
has been queued. Poll `app_state.state.send_in_progress` until it becomes false
before checking `last_send_report`, `last_error`, or send-related file changes.
If the primary window closes while a send is pending, the application remains
alive until send finalization completes.

`close_main_window` schedules closure of only the primary window after returning
its response. Dirty compose state can first defer it behind a confirmation.
Outstanding send, sync, or tag work, a tag-warning reconciliation search, or an
active draft save instead hides the window and defers closure until that work
settles; the latest recovery-draft state is then flushed asynchronously before
the window closes. A failed reconciliation or recovery-draft flush re-presents
the window and reports the error. Standalone message windows, if any, are left
open. Attachment workers hold the application independently: the primary window
may close while a save finishes, but the process remains alive until the worker
publishes the complete file or reports failure.

`set_layout` accepts `auto`, `columns` (with `three_pane` kept as a
compatibility spelling), and `stacked`. `toggle_layout` cycles through columns,
stacked, and auto.

The `screenshot` command writes to `artifacts/screenshots/` by default. The app
tries desktop screenshot tools when native capture is unavailable and reports
errors instead of faking screenshots.

See also `notm-test-harness(7)`.
