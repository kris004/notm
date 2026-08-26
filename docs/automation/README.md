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
Fixture harnesses may add `test_delay_ms` (up to 5000) when testing that the UI
and harness remain responsive during an outstanding search. The delay option is
rejected for non-fixture runs.

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

- health/state: `health`, `app_state`, `search_status`, `get_logs`, `screenshot`
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
  `forward_as_attachment_selected`
- address/drafts/attachments: `get_address_suggestions`,
  `select_address_suggestion_by_index`, `autocomplete_recipient`,
  `attachment_list_items`, `select_attachment_by_index`, `save_draft`,
  `list_drafts`, `select_draft_by_index`, `load_selected_draft`,
  `delete_selected_draft`, `delete_active_draft`, `delete_local_draft`,
  `load_draft`, `clear_draft`, `draft_list_state`,
  `activate_draft_by_index`, `click_delete_selected_draft`,
  `pending_confirmation`, `respond_confirmation`,
  `save_selected_attachment`, `save_attachment`, `open_selected_attachment`,
  `open_attachment`, `attachment_test_state`, `respond_attachment_save`
- message actions: `show_raw_source`, `open_raw_source`, `show_full_headers`,
  `full_headers`, `show_text_thread`, `show_rendered_thread`,
  `toggle_text_visual`, `show_visual_html`, `show_html_visual`, `image_policy`,
  `load_images_once`, `html_view_state`, `html_scroll_state`,
  `scroll_html_view_lines`,
  `view_preference_state`, `click_sender_view_preference`,
  `start_link_hints`, `link_hint_state`, `input_link_hint`,
  `cancel_link_hints`, `toggle_quote_collapse`, `message_view_text`,
  `copy_message_id`, `copy_thread_id`
- UI/debug: `send_key`, `open_command_palette`, `command_completion`,
  `open_shortcuts`, `show_shortcuts`, `help_search`, `run_command`,
  `run_manual_sync`, `open_settings`, `settings_test_state`,
  `respond_settings`, `save_settings`, `resize_window`, `pane_visibility`,
  `set_pane_visibility`, `layout_state`,
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

`click_message_tag_action` drives the real current-message menu buttons and
accepts `action` set to `archive`, `read`, `flag`, `trash`, `spam`, or `custom`.
The custom action also accepts `tag`. These operations target the selected
message ID only; thread selection and multi-selection are ignored. Like the
thread tag commands, they require fixture mode or
`automation.allow_live_tag_test=true`.

`load_images_once` reloads Visual HTML with remote images enabled only for the
current message view. It creates no durable sender permission and resets when
the test navigates away or restarts the application.

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
action is exactly `send_composer`; this narrow gate lets custom-transport smokes
drive the real saved-draft Send modal and does not expose other live
confirmations.

`save_selected_attachment` and its `save_attachment` compatibility spelling
open the same GTK save chooser as the normal UI when `dir` is omitted. The
fixture-only `attachment_test_state` reports the pending chooser ID, sanitized
suggested name, visible status, private Open directory, and fake-opener calls.
Respond deterministically with
`respond_attachment_save {"id": ID, "response": "accept", "path":
"/full/target"}` or with `"response": "cancel"`. Accept treats that complete
target as authoritative and creates a numbered sibling rather than replacing
an existing file; cancel is a successful no-op. Supplying an explicit `dir` to
`save_selected_attachment` remains a synchronous storage-test bypass and does
not exercise the chooser.

Attachment Open writes a safely named file beneath a private, mode-0700
application directory before launching it. Fixture mode records the path in a
fake opener instead of starting an external application. The directory and its
files remain available while the app runs and are removed when the process
exits normally.

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
runtime state or the config file. `apply` changes the running window without
writing; `save` writes and then applies. Send changes require relaunch. The
older `save_settings` command persists only its basic direct-test fields and
does not exercise the dialog.

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

`close_main_window` closes only the primary window after returning its response,
unless dirty compose state first defers it behind a confirmation. Standalone
message windows, if any, are left open.

`set_layout` accepts `auto`, `columns` (with `three_pane` kept as a
compatibility spelling), and `stacked`. `toggle_layout` cycles through columns,
stacked, and auto.

The `screenshot` command writes to `artifacts/screenshots/` by default. The app
tries desktop screenshot tools when native capture is unavailable and reports
errors instead of faking screenshots.

See also `notm-test-harness(7)`.
