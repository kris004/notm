# Send transport

`notm` does not implement SMTP itself. It builds a transport abstraction for external commands with modes:

- `stdin_rfc5322`
- `file_arg`
- `command_template`
- `auto`

Auto mode only runs harmless probes and never sends mail. Fake transport tests prove valid RFC5322 bytes, including attachment MIME parts, before any real transport is used.

The live UI refuses to fall back to the fake capture transport when no
`send.command` is configured. Fake capture is only enabled for fixture/test
launches so a real compose window cannot report success for an unsent message.

Transport mode behavior:

- `auto`: currently the same as `stdin_rfc5322`. notm writes the complete RFC5322 message to the command's standard input and passes exactly the configured `send.args`.
- `stdin_rfc5322`: writes the complete RFC5322 message to standard input. Use this for sendmail-style commands that read the message from stdin.
- `file_arg`: writes the complete RFC5322 message to a temporary file, then appends that file path after all configured `send.args`.
- `command_template`: writes the complete RFC5322 message to a temporary file, then replaces `{file}` wherever it appears in each configured argument. Use this when the command needs the message path in a specific position, such as `args = ["--message", "{file}"]`. This mode fails if no argument contains `{file}`.

Any helper-specific flags must be set explicitly in config. notm does not inspect helper scripts or add implicit arguments.

Optional post-send persistence is explicit:

```toml
[send]
save_sent = true
sent_maildir = "/path/to/Mail/Sent" # optional; defaults to <database>/Sent when enabled
sent_tags = ["sent"]
index_sent_after_send = true
```

If enabled, `notm` writes the exact RFC5322 message to a Maildir and, only when `index_sent_after_send=true`, indexes that one file through libnotmuch and applies the configured tags. It does not run `notmuch new` or any sync command.

Explicit draft saves are Maildir-backed by default. They write a normal local
message under `<database>/Drafts`, index that file, and apply `tag:draft`; set
`save_maildir = false` to fall back to local JSON draft files. Because this is a
local Maildir file, Gmail does not auto-delete it: use `Delete local draft` while
editing the draft to remove the file and its notmuch index entry. Opening a
message tagged with the configured draft tag opens it in the composer for later
editing/sending.

```toml
[drafts]
save_maildir = true
maildir = "/path/to/Mail/Drafts" # optional; defaults to <database>/Drafts when enabled
tags = ["draft"]
index_after_save = true
```

Live send validation is explicitly gated and should only be run against a user-configured throwaway or personal test transport.
