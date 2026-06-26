# AGENTS.md

Conservative repo guidance for automated contributors.

- Start with `docs/testing.md` for routine checks, smoke tests, and runtime
  debugging guidance.
- When fixing a bug, add or extend a targeted test or smoke check when practical
  so the same regression is harder to reintroduce. If that is not practical,
  explain why in the change notes.
- Do not add or loosen Clippy allowances for expediency. Use an allowance only
  when it is necessary, narrow, and justified by the code.
