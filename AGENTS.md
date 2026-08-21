# AGENTS.md

Conservative repo guidance for automated contributors.

- Start with `docs/testing.md` for routine checks, smoke tests, and runtime
  debugging guidance.
- When fixing a bug, add or extend a targeted test or smoke check when practical
  so the same regression is harder to reintroduce. If that is not practical,
  explain why in the change notes.
- Do not add or loosen Clippy allowances for expediency. Use an allowance only
  when it is necessary, narrow, and justified by the code.
- Treat every file, commit, branch, tag, and reachable history as public. Do not
  commit secrets, private network details, personal data, private issue links,
  environment identifiers, or unnecessary absolute local paths.
- Keep documentation, examples, fixtures, and defaults portable for external
  users and contributors.
- Preserve unrelated dirty work and stage exact paths.
- Before exposing any previously private history, scan both the current tree
  and reachable Git history for private or environment-specific material.
- Do not change visibility, push, publish, or create a release without explicit
  authorization.
