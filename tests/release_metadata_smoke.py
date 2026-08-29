#!/usr/bin/env python3
"""Deterministic negative coverage for the release metadata verifier."""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parent.parent
VERIFIER = PROJECT_ROOT / "packaging/verify-release-metadata.py"
FIXTURE_FILES = (
    ".gitattributes",
    ".git_archival.txt",
    "Cargo.lock",
    "Cargo.toml",
    "CHANGELOG.md",
    "Makefile",
    "crates/notm-app/Cargo.toml",
    "crates/notm-mail/Cargo.toml",
    "crates/notm-notmuch/Cargo.toml",
    "crates/notm-test-support/Cargo.toml",
    "crates/notm-ui/Cargo.toml",
    "docs/man/notm.1",
    "docs/man/notm-automation.7",
    "docs/man/notm-config.5",
    "docs/man/notm-test-harness.7",
    "packaging/io.github.kris004.notm.desktop",
    "packaging/io.github.kris004.notm.metainfo.xml",
)


def run_verifier(root: Path, *arguments: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-B", str(VERIFIER), *arguments, str(root)],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def replace_once(path: Path, old: str, new: str) -> None:
    contents = path.read_text(encoding="utf-8")
    if contents.count(old) != 1:
        raise AssertionError(f"expected one occurrence of {old!r} in {path}")
    path.write_text(contents.replace(old, new, 1), encoding="utf-8")


def set_archive_provenance(root: Path, commit: str, commit_date: str) -> None:
    path = root / ".git_archival.txt"
    lines = path.read_text(encoding="utf-8").splitlines()
    commit_lines = sum(line.startswith("commit=") for line in lines)
    date_lines = sum(line.startswith("commit-date=") for line in lines)
    if commit_lines != 1 or date_lines != 1:
        raise AssertionError(f"unexpected archive metadata structure in {path}")
    rewritten = [
        f"commit={commit}" if line.startswith("commit=") else
        f"commit-date={commit_date}" if line.startswith("commit-date=") else
        line
        for line in lines
    ]
    path.write_text("\n".join(rewritten) + "\n", encoding="utf-8")


def assert_rejected(root: Path, expected_error: str) -> None:
    result = run_verifier(root)
    if result.returncode == 0:
        raise AssertionError(f"verifier accepted inconsistent fixture: {expected_error}")
    if expected_error not in result.stderr:
        raise AssertionError(
            f"missing error {expected_error!r}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="notm-release-metadata.") as temporary:
        work_root = Path(temporary)
        pristine = work_root / "pristine"
        for relative in FIXTURE_FILES:
            destination = pristine / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(PROJECT_ROOT / relative, destination)

        baseline = run_verifier(pristine, "--expected-version", "0.1.2")
        if baseline.returncode != 0:
            raise AssertionError(f"baseline metadata failed:\n{baseline.stderr}")

        cases = (
            (
                "lock-version",
                "Cargo.lock",
                'name = "notm-app"\nversion = "0.1.2"',
                'name = "notm-app"\nversion = "9.9.9"',
                "Cargo.lock version mismatch for notm-app",
            ),
            (
                "changelog-version",
                "CHANGELOG.md",
                "## [0.1.2] - 2026-08-28",
                "## [9.9.9] - 2026-08-28",
                "latest changelog version mismatch",
            ),
            (
                "man-version",
                "docs/man/notm.1",
                '"notm 0.1.2"',
                '"notm 9.9.9"',
                "docs/man/notm.1 version mismatch",
            ),
            (
                "appstream-version",
                "packaging/io.github.kris004.notm.metainfo.xml",
                '<release version="0.1.2" date="2026-08-28">',
                '<release version="9.9.9" date="2026-08-28">',
                "latest AppStream version mismatch",
            ),
            (
                "desktop-exec",
                "packaging/io.github.kris004.notm.desktop",
                "Exec=notm launch %u",
                "Exec=notm %u",
                "desktop Exec mismatch",
            ),
            (
                "package-id",
                "Makefile",
                "DESKTOP_ID := io.github.kris004.notm",
                "DESKTOP_ID := invalid.example.notm",
                "Makefile DESKTOP_ID must be",
            ),
            (
                "crate-version-source",
                "crates/notm-mail/Cargo.toml",
                "version.workspace = true",
                'version = "9.9.9"',
                "must inherit package.version from the workspace",
            ),
        )
        for name, relative, old, new, expected_error in cases:
            case_root = work_root / name
            shutil.copytree(pristine, case_root)
            replace_once(case_root / relative, old, new)
            assert_rejected(case_root, expected_error)

        unexpanded_root = work_root / "unexpanded"
        shutil.copytree(pristine, unexpanded_root)
        set_archive_provenance(unexpanded_root, "$Format:%H$", "$Format:%cI$")
        unexpanded = run_verifier(unexpanded_root, "--require-archive-provenance")
        if unexpanded.returncode == 0 or "placeholder was not expanded" not in unexpanded.stderr:
            raise AssertionError("archive mode accepted unexpanded Git placeholders")

        archive_root = work_root / "archive"
        shutil.copytree(pristine, archive_root)
        commit = "a" * 40
        set_archive_provenance(archive_root, commit, "2026-08-25T00:00:00+00:00")
        archive_result = run_verifier(
            archive_root,
            "--require-archive-provenance",
            "--expected-source-commit",
            commit,
        )
        if archive_result.returncode != 0:
            raise AssertionError(f"expanded archive metadata failed:\n{archive_result.stderr}")
        wrong_commit = run_verifier(
            archive_root,
            "--require-archive-provenance",
            "--expected-source-commit",
            "b" * 40,
        )
        if (
            wrong_commit.returncode == 0
            or "source archive commit mismatch" not in wrong_commit.stderr
        ):
            raise AssertionError("archive verifier accepted the wrong expected commit")

        naive_date_root = work_root / "naive-date"
        shutil.copytree(archive_root, naive_date_root)
        set_archive_provenance(naive_date_root, commit, "2026-08-25T00:00:00")
        naive_date = run_verifier(
            naive_date_root,
            "--require-archive-provenance",
            "--expected-source-commit",
            commit,
        )
        if (
            naive_date.returncode == 0
            or "must include a UTC offset" not in naive_date.stderr
        ):
            raise AssertionError("archive verifier accepted a timezone-naive commit date")

    print("release_metadata_smoke ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
