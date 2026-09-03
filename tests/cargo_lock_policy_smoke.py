#!/usr/bin/env python3
"""Reject resolver-capable Cargo commands that do not use Cargo.lock."""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parent.parent
LOCKED_COMMANDS = (
    "bench",
    "build",
    "check",
    "clippy",
    "doc",
    "fetch",
    "install",
    "metadata",
    "package",
    "run",
    "test",
)
CARGO_COMMAND = re.compile(
    rf"(?<![\w.-])(?:cargo|\$\(CARGO\))\s+"
    rf"(?P<command>{'|'.join(LOCKED_COMMANDS)})\b"
)
RUST_CARGO_COMMAND = re.compile(
    r'Command::new\(env!\("CARGO"\)\)(?P<body>.*?)(?:\.status\(\)|\.output\(\))',
    re.DOTALL,
)
RUST_COMMAND_ARGUMENT = re.compile(
    rf'"(?P<command>{"|".join(LOCKED_COMMANDS)})"'
)
COMMAND_SEPARATOR = re.compile(r"&&|\|\||[;|]")
SOURCE_ARCHIVE_TEST_COMMAND = (
    "cargo test --locked --workspace --all-targets --all-features -- "
    "--test-threads=1"
)


@dataclass(frozen=True)
class Violation:
    path: Path
    line_number: int
    command: str
    line: str


def unlocked_commands(path: Path, text: str) -> list[Violation]:
    """Return Cargo commands lacking --locked on their physical command line."""

    violations: list[Violation] = []
    for line_number, line in enumerate(text.splitlines(), start=1):
        for match in CARGO_COMMAND.finditer(line):
            suffix = line[match.end() :]
            command_tail = COMMAND_SEPARATOR.split(suffix, maxsplit=1)[0]
            cargo_arguments = command_tail.split()
            if "--" in cargo_arguments:
                cargo_arguments = cargo_arguments[: cargo_arguments.index("--")]
            normalized_arguments = {
                argument.strip("`'\"").rstrip(".,") for argument in cargo_arguments
            }
            if "--locked" not in normalized_arguments:
                violations.append(
                    Violation(path, line_number, match.group("command"), line.strip())
                )
    return violations


def unlocked_rust_commands(path: Path, text: str) -> list[Violation]:
    """Check nested Cargo invocations expressed through std::process::Command."""

    violations: list[Violation] = []
    for match in RUST_CARGO_COMMAND.finditer(text):
        body = match.group("body")
        command_match = RUST_COMMAND_ARGUMENT.search(body)
        if command_match is None or '"--locked"' in body:
            continue
        line_number = text.count("\n", 0, match.start()) + 1
        violations.append(
            Violation(
                path,
                line_number,
                command_match.group("command"),
                text.splitlines()[line_number - 1].strip(),
            )
        )
    return violations


def source_archive_test_commands(text: str) -> list[str]:
    """Return normalized Cargo test commands from the archive smoke."""

    logical_text = re.sub(r"\\\n[ \t]*", " ", text)
    return [
        " ".join(line.split())
        for line in logical_text.splitlines()
        if re.match(r"^[ \t]*cargo test\b", line)
    ]


def run_parser_regressions() -> None:
    """Prove the scanner catches the unlocked-first-command regression."""

    old_release_resolver = """\
cargo metadata --no-deps --format-version 1 |
  jq -r '.packages[0].version'
cargo build --release --locked -p notm-app
"""
    violations = unlocked_commands(Path("old-release.yml"), old_release_resolver)
    actual = [(item.line_number, item.command) for item in violations]
    if actual != [(1, "metadata")]:
        raise AssertionError(f"unlocked-first-command regression escaped: {actual}")

    chained_commands = (
        "cargo test --workspace && cargo build --release --locked -p notm-app"
    )
    violations = unlocked_commands(Path("chained.sh"), chained_commands)
    actual = [(item.line_number, item.command) for item in violations]
    if actual != [(1, "test")]:
        raise AssertionError(f"unlocked chained command escaped: {actual}")

    make_command = "$(CARGO) run --locked -p notm-app -- fixture-smoke"
    violations = unlocked_commands(Path("Makefile"), make_command)
    if violations:
        raise AssertionError(f"locked Make command was rejected: {violations}")

    misplaced_lock = "cargo run -p notm-app -- --locked fixture-smoke"
    violations = unlocked_commands(Path("misplaced.sh"), misplaced_lock)
    actual = [(item.line_number, item.command) for item in violations]
    if actual != [(1, "run")]:
        raise AssertionError(f"post-separator --locked was accepted: {actual}")

    rust_command = '''\
let status = Command::new(env!("CARGO"))
    .args(["run", "--quiet", "--", "fixture-smoke"])
    .status();
'''
    violations = unlocked_rust_commands(Path("nested.rs"), rust_command)
    actual = [(item.line_number, item.command) for item in violations]
    if actual != [(1, "run")]:
        raise AssertionError(f"unlocked nested Rust command escaped: {actual}")

    parallel_archive_test = (
        "cargo test --locked --workspace --all-targets --all-features"
    )
    if source_archive_test_commands(parallel_archive_test) == [
        SOURCE_ARCHIVE_TEST_COMMAND
    ]:
        raise AssertionError("parallel source-archive test command was accepted")

    if source_archive_test_commands(SOURCE_ARCHIVE_TEST_COMMAND) != [
        SOURCE_ARCHIVE_TEST_COMMAND
    ]:
        raise AssertionError("serialized source-archive test command was rejected")


def policy_paths() -> list[Path]:
    paths = [PROJECT_ROOT / "Makefile", PROJECT_ROOT / "README.md"]
    paths.extend(sorted(PROJECT_ROOT.glob("*.md")))
    paths.extend(sorted((PROJECT_ROOT / "docs").rglob("*.md")))
    paths.extend(sorted((PROJECT_ROOT / ".github" / "workflows").glob("*.yml")))
    paths.extend(sorted((PROJECT_ROOT / ".github" / "workflows").glob("*.yaml")))
    paths.extend(sorted((PROJECT_ROOT / "packaging").rglob("*.sh")))
    paths.extend(sorted((PROJECT_ROOT / "tests").glob("*.sh")))
    paths.extend(sorted((PROJECT_ROOT / "tests").glob("*.rs")))
    return sorted(set(paths))


def main() -> int:
    run_parser_regressions()

    source_archive_path = PROJECT_ROOT / "tests" / "source_archive_smoke.sh"
    archive_test_commands = source_archive_test_commands(
        source_archive_path.read_text(encoding="utf-8")
    )
    if archive_test_commands != [SOURCE_ARCHIVE_TEST_COMMAND]:
        print(
            "source archive smoke must run the complete locked workspace test "
            "exactly once with --test-threads=1",
            file=sys.stderr,
        )
        print(f"  found: {archive_test_commands}", file=sys.stderr)
        return 1

    paths = policy_paths()
    missing = [path for path in paths if not path.is_file()]
    if missing:
        for path in missing:
            print(f"required policy input is missing: {path}", file=sys.stderr)
        return 1

    violations: list[Violation] = []
    command_count = 0
    scope_counts = {"Makefile": 0, "workflows": 0, "docs": 0}
    for path in paths:
        text = path.read_text(encoding="utf-8")
        matches = list(CARGO_COMMAND.finditer(text))
        command_count += len(matches)
        if path.name == "Makefile":
            scope_counts["Makefile"] += len(matches)
        elif ".github/workflows" in path.as_posix():
            scope_counts["workflows"] += len(matches)
        else:
            scope_counts["docs"] += len(matches)
        violations.extend(unlocked_commands(path, text))
        violations.extend(unlocked_rust_commands(path, text))

    empty_scopes = [scope for scope, count in scope_counts.items() if count == 0]
    if empty_scopes:
        print(
            "Cargo lock policy found no commands in expected scope(s): "
            + ", ".join(empty_scopes),
            file=sys.stderr,
        )
        return 1

    if violations:
        for item in violations:
            relative_path = item.path.relative_to(PROJECT_ROOT)
            print(
                f"{relative_path}:{item.line_number}: cargo {item.command} must "
                "pass --locked on the same command line",
                file=sys.stderr,
            )
            print(f"  {item.line}", file=sys.stderr)
        return 1

    print(
        f"Cargo lock policy passed: {command_count} commands across "
        f"{len(paths)} files"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
