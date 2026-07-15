# Developer test harness

This document lives under `docs/automation/` for historical reasons: older
flags, config keys, and internal module names used the word "automation". The
current user-facing name is **developer test harness**.

This is not mail automation, filtering, or rules support. It is a local
UI-driving test harness for developers and AI agents working on `notm`.

Normal users do not need the test harness enabled to use `notm`; the public
README intentionally stays focused on the desktop mail experience.

The harness uses a local Unix-domain socket. It is disabled by default and should
be enabled only for a controlled local run. Requests are JSON lines containing
`token`, `command`, and optional `args`. Commands are dispatched into the GTK
main loop and operate on the real app model/widgets.

## Launch with the test harness

```sh
cargo run -p notm-app -- launch \
  --test-harness \
  --test-harness-socket /tmp/notm.sock \
  --test-harness-token dev-token
```

Against disposable fixture data:

```sh
cargo run -p notm-app -- launch \
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
- Draft save/delete and attachment operations use the configured local paths;
  they are not covered by the live send/tag gates and should be driven only
  when those local file changes are intentional.
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
  `select_message_by_index`, `thread_ui_details`, `toggle_multi_select_thread`,
  `clear_multi_selection`
- tags: `archive_selected`, `mark_read_selected`, `mark_unread_selected`,
  `flag_selected`, `unflag_selected`, `trash_selected`, `spam_selected`,
  `tag_selected`, `add_tag_selected`, `remove_tag_selected`, `undo_last_tag`,
  `undo_tag_actions`
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
  `load_draft`, `clear_draft`, `save_selected_attachment`,
  `open_selected_attachment`, `open_attachment`
- message actions: `show_raw_source`, `open_raw_source`, `show_full_headers`,
  `full_headers`, `show_text_thread`, `show_rendered_thread`,
  `toggle_text_visual`, `show_visual_html`, `show_html_visual`, `image_policy`,
  `load_images_once`, `trust_sender_images`, `trusted_image_senders`,
  `html_view_state`, `html_scroll_state`, `scroll_html_view_lines`,
  `toggle_quote_collapse`, `message_view_text`, `copy_message_id`,
  `copy_thread_id`
- UI/debug: `open_command_palette`, `command_completion`, `open_shortcuts`,
  `show_shortcuts`, `help_search`, `run_command`, `run_manual_sync`,
  `open_settings`, `save_settings`, `resize_window`, `pane_visibility`,
  `set_pane_visibility`, `layout_state`, `set_layout`, `toggle_layout`,
  `toggle_debug_panel`, `close_main_window`, custom saved-search commands, and
  custom tag-editor commands

`focus_search` and `focus_compose_field` move GTK focus without forcing Insert
mode. Tests that exercise keyboard editing should send the same `/`, `i`, or
Enter transition that a user would use instead of relying on the harness to
change modes implicitly.

`run_manual_sync` returns with `pending: true` after the configured commands
have started in the background. Poll `app_state.state.sync_in_progress` until
it becomes false before asserting the command result or refreshed mail state;
the flag remains true until the post-sync search refresh also finishes.
Non-fixture harness checks may pass `test_refresh_delay_ms` (maximum 5000) to
keep that refresh pending while they exercise responsiveness and cancellation
behavior; normal UI and command-palette syncs never add this delay.

`compose_send` returns with `pending: true` after the composed snapshot has been
queued. Poll `app_state.state.send_in_progress` until it becomes false before
checking `last_send_report`, `last_error`, or send-related file changes. If the
primary window closes while a send is pending, the application remains alive
until send finalization completes.

`close_main_window` closes only the primary window after returning its response.
Standalone message windows, if any, are left open.

`set_layout` accepts `auto`, `columns` (with `three_pane` kept as a
compatibility spelling), and `stacked`. `toggle_layout` cycles through columns,
stacked, and auto.

The `screenshot` command writes to `artifacts/screenshots/` by default. The app
tries desktop screenshot tools when native capture is unavailable and reports
errors instead of faking screenshots.

See also `notm-test-harness(7)`.
