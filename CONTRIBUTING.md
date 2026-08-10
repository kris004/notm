# Contributing to notm

Thanks for taking the time to improve `notm`. The project is still small, so
focused issues and pull requests are easier to review than broad rewrites.

## Before opening an issue

- Search the existing issues first.
- Include the `notm` version or commit, Linux distribution, and relevant GTK,
  WebKitGTK, and Notmuch versions.
- Describe the shortest sequence that reproduces the problem and what you
  expected to happen.
- Use synthetic mail whenever possible. Remove real addresses, subjects,
  message IDs, bodies, attachment contents, paths, tokens, and command arguments
  from screenshots and logs.
- Use the private process in [SECURITY.md](SECURITY.md) for security problems.

For a large feature or user-visible behavior change, open an issue before doing
substantial implementation work so the scope can be agreed before code is
written.

## Build

The native dependencies and basic build instructions are in the
[README](README.md#requirements). A development build can be started with:

```sh
cargo run -p notm-app -- launch
```

## Test

Use disposable fixture data before a live mailbox. The routine checks are:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo run -p notm-app -- fixture-smoke
```

Run `./tests/packaging_install_smoke.sh` when changing installation or packaging
files. UI changes should also run the narrowest relevant non-skipping GTK smoke
test on a real or virtual display. See [docs/testing.md](docs/testing.md) for the
full commands and the difference between fixture and live tests.

Do not run `live-readonly-smoke` or `live-self-send` against a real setup unless
that access is intentional. `live-self-send` sends real mail.

## Pull requests

- Keep the change scoped and avoid unrelated cleanup.
- Add or update a focused test when behavior changes.
- Update user documentation when configuration, commands, or visible behavior
  changes.
- Do not weaken warnings or add broad Clippy allowances to make a check pass.
- Explain what was tested, including whether a GTK test ran with a required
  display rather than skipping.

By submitting a contribution, you agree that it may be distributed under the
repository's [GPL-3.0-or-later license](LICENSE).
