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

For configured helper scripts whose contents invoke `gmi send`, auto mode uses stdin-RFC5322 and appends `-t` when no explicit recipient/template args are configured, because `gmi send` requires `-t` to trust RFC5322 recipients from headers.

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

The completed live send validations used bounded unique subjects with prefix `notm validation self-test`. Final polish did not send additional live mail because fixture/fake transport covered the remaining send/indexing behavior.
