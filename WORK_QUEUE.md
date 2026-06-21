# notm remaining work queue

This file is the compaction-safe tracker for finishing the remaining daily-driver gaps one at a time. Rule: finish a gap, run its quality gate, update PROGRESS.md and TEST_REPORT.md, then move to the next gap.

## Active policy

- Do not run receive/sync commands.
- Do not run `notmuch` CLI for app/test behavior.
- Live send validation is allowed by user; use consistent subject prefix `notm validation self-test` and record every subject/count.
- Prefer bounded live batches; no unbounded loops.
- Keep artifacts in `artifacts/`.

## Queue

1. [x] Live GTK UI send validation with consistent subject prefix.
2. [x] Settings/custom saved-search/tag-editor UI: editable app settings view, custom saved-search persistence, GUI custom add/remove tag.
3. [x] Drafts/address UI polish: multi-draft local manager plus recipient dropdown/chip-like suggestions.
4. [x] Performance hardening: debounce search input, stale-search cancellation token/generation, async Notmuch worker where practical, cache keyed by query+revision.
5. [x] Thread/message indicators and viewer toggles: attachment/encrypted/signed indicators, previews, full headers/raw/source/rendered toggles, quote collapse.
6. [x] Forward-as-attachment and optional sent/draft indexing paths when explicitly configured.
7. [x] Final full validation and report.

## Current checkpoint

Gap 1 complete: sent two live UI self-test emails through the real GTK automation path with prefix `notm validation self-test`; both were accepted and indexed without forced sync. Gap 2 complete: editable settings persistence, custom saved-search save/delete/select, and GUI custom tag add/remove validated against fixture GTK app. Gap 3 complete: multi-draft manager and visible address suggestion selection validated. Gap 4 complete: debounced background search, stale-generation discard, and query+revision cache validated. Gap 5 complete: thread indicators and viewer toggles validated. Gap 6 complete: forward-as-RFC822-attachment plus explicitly configured sent/draft Maildir save and native Notmuch indexing validated. Gap 7 complete: final fmt/clippy/tests/fixture smoke/send probe/live read-only smoke and final GTK automation screenshot passed. All tracked gaps are complete.

## Post-final partial-gap closure

- [x] Large inbox: real result paging, Load more UI/automation, count/loaded state, offset-aware cache; validated with `ui.page_size = 3` and screenshot `28_large_inbox_paging_load_more.png`.
- [x] Actual HTML rendering: WebKitGTK 6 visual HTML view with sanitized HTML, JavaScript/navigation/file access disabled, remote images disabled by default, automation state, and screenshot `29_webkit_html_visual.png`.

Current checkpoint: the two specifically called-out partial gaps, large-inbox behavior and actual visual HTML rendering, are implemented and passed their feature gates.

- [x] Remote image controls: one-shot image loading, persisted trusted sender image allow-list, and normal-policy re-render validation.
- [x] Enter-key regression: global key controller now captures before focused toolbar buttons, so Enter opens the selected thread instead of activating Compose.
- [x] Scroll-bottom paging: thread list scroll position automatically loads the next page; automation exposes `scroll_thread_list_to_bottom`.
