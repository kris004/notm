# Security and privacy

- `notm` is local-only and has no telemetry.
- HTML mail is sanitized. The default rendered view is safe text; the Visual HTML view uses WebKitGTK with JavaScript and in-app navigation blocked, file/universal file access disabled, and remote images disabled by default. Images can be loaded for one view or for a persisted trusted sender only after explicit user action.
- The automation socket is disabled by default, local-only, and token-gated.
- Logs redact bodies by default.
- The app never deletes ordinary message files through trash/spam/archive; those are tag operations only. The composer has an explicit `Delete local draft` action for locally saved draft files.
- Sync/receive commands are disabled by default and only run when explicitly configured and manually invoked.
- Draft recovery stores local JSON in the user cache. Explicit draft saves write local Maildir messages tagged `draft` by default; those local drafts can be reopened in the composer and explicitly deleted locally.
- Optional sent Maildir saving/indexing is disabled by default and only runs after an accepted send when configured.
- External sending is explicit and configurable. Fake transport tests are used before live transport, and live-send validation is bounded with recorded subjects.
- Visual HTML link navigation is blocked in-app and the status bar shows the target. Link opening and attachment opening via desktop defaults should be treated as user-initiated actions.
