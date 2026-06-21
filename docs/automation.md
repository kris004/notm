# Automation

Automation is a local Unix-domain socket, disabled by default. Enable it explicitly:

```sh
cargo run -p notm-app -- launch --automation --automation-socket /tmp/notm.sock --automation-token dev-token
```

Requests are JSON lines containing `token`, `command`, and optional `args`. Commands are dispatched into the GTK main loop and operate on the real app model/widgets.

Example:

```json
{"token":"dev-token","command":"run_search","args":{"query":"tag:inbox"}}
```

Implemented commands include:

- health/state: `health`, `app_state`, `get_logs`, `screenshot`
- search/navigation: `focus_search`, `set_search_query`, `run_search`, `load_more_threads`, `scroll_thread_list_to_bottom`, `thread_page_info`, `select_saved_search`, `select_thread_by_index`, `open_selected_thread`, `select_message_by_index`, `thread_ui_details`
- tags: `archive_selected`, `mark_read_selected`, `mark_unread_selected`, `flag_selected`, `unflag_selected`, `trash_selected`, `spam_selected`, `tag_selected`, `add_tag_selected`, `remove_tag_selected`, `undo_last_tag`
- compose/send: `open_compose`, `compose_set_from`, `compose_set_to`, `compose_set_cc`, `compose_set_bcc`, `compose_set_subject`, `compose_set_body`, `compose_add_attachment`, `compose_send`
- replies/forwards: `reply_selected`, `reply_all_selected`, `forward_selected`, `forward_as_attachment_selected`
- address/drafts/attachments: `get_address_suggestions`, `select_address_suggestion_by_index`, `autocomplete_recipient`, `attachment_list_items`, `select_attachment_by_index`, `save_draft`, `list_drafts`, `select_draft_by_index`, `load_selected_draft`, `delete_selected_draft`, `load_draft`, `clear_draft`, `save_selected_attachment`, `open_selected_attachment`, `open_attachment`
- message actions: `show_raw_source`, `open_raw_source`, `show_full_headers`, `full_headers`, `show_rendered_thread`, `show_visual_html`, `show_html_visual`, `load_images_once`, `trust_sender_images`, `trusted_image_senders`, `html_view_state`, `toggle_quote_collapse`, `message_view_text`, `copy_message_id`, `copy_thread_id`
- UI/debug: `open_command_palette`, `open_shortcuts`, `show_shortcuts`, `run_command`, `run_manual_sync`, `open_settings`, `save_settings`, `toggle_debug_panel`, custom saved-search and custom tag editor commands

Screenshots are written to `artifacts/screenshots/` by default. The app tries desktop screenshot tools when native capture is unavailable and reports errors instead of faking screenshots.
