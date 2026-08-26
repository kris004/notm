#!/usr/bin/env python3
"""Verify that notm's duplicated release metadata remains consistent."""

from __future__ import annotations

import argparse
import datetime as dt
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path


APP_ID = "io.github.kris004.notm"
SEMVER_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[.-][0-9A-Za-z.-]+)?$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
COMMIT_PLACEHOLDER = "$Format:%H$"
DATE_PLACEHOLDER = "$Format:%cI$"
MAN_PAGES = (
    "docs/man/notm.1",
    "docs/man/notm-config.5",
    "docs/man/notm-test-harness.7",
    "docs/man/notm-automation.7",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source_root", nargs="?", default=".")
    parser.add_argument("--expected-version")
    parser.add_argument("--expected-source-commit")
    parser.add_argument("--require-archive-provenance", action="store_true")
    parser.add_argument("--print-version", action="store_true")
    return parser.parse_args()


def read_text(root: Path, relative: str, errors: list[str]) -> str:
    path = root / relative
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        errors.append(f"cannot read {relative}: {error}")
        return ""


def section_value(text: str, section: str, key: str) -> str | None:
    current = None
    for line in text.splitlines():
        stripped = line.strip()
        match = re.fullmatch(r"\[\[?([^]]+)]]?", stripped)
        if match:
            current = match.group(1)
            continue
        if current != section:
            continue
        match = re.fullmatch(rf"{re.escape(key)}\s*=\s*\"([^\"]+)\"", stripped)
        if match:
            return match.group(1)
    return None


def section_bool(text: str, section: str, key: str) -> bool | None:
    current = None
    for line in text.splitlines():
        stripped = line.strip()
        match = re.fullmatch(r"\[\[?([^]]+)]]?", stripped)
        if match:
            current = match.group(1)
            continue
        if current != section:
            continue
        match = re.fullmatch(rf"{re.escape(key)}\s*=\s*(true|false)", stripped)
        if match:
            return match.group(1) == "true"
    return None


def parse_lock_packages(text: str) -> list[dict[str, str]]:
    packages: list[dict[str, str]] = []
    for block in re.split(r"(?m)^\[\[package]]\s*$", text)[1:]:
        package: dict[str, str] = {}
        for key in ("name", "version", "source"):
            match = re.search(rf'(?m)^{key}\s*=\s*"([^"]+)"\s*$', block)
            if match:
                package[key] = match.group(1)
        packages.append(package)
    return packages


def validate_iso_date(value: str, label: str, errors: list[str]) -> None:
    try:
        dt.date.fromisoformat(value)
    except ValueError:
        errors.append(f"{label} is not an ISO date: {value}")


def validate_iso_datetime(value: str, label: str, errors: list[str]) -> None:
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        errors.append(f"{label} is not an ISO date-time: {value}")
        return
    if parsed.tzinfo is None or parsed.utcoffset() is None:
        errors.append(f"{label} must include a UTC offset: {value}")


def parse_desktop(text: str) -> dict[str, str]:
    values: dict[str, str] = {}
    in_desktop_entry = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            in_desktop_entry = stripped == "[Desktop Entry]"
            continue
        if in_desktop_entry and "=" in stripped and not stripped.startswith("#"):
            key, value = stripped.split("=", 1)
            values[key] = value
    return values


def verify(root: Path, args: argparse.Namespace) -> tuple[str | None, list[str]]:
    errors: list[str] = []
    cargo_toml = read_text(root, "Cargo.toml", errors)
    version = section_value(cargo_toml, "workspace.package", "version")
    if version is None:
        errors.append("Cargo.toml is missing workspace.package.version")
    elif not SEMVER_RE.fullmatch(version):
        errors.append(f"workspace version is not release-shaped: {version}")
    if args.expected_version and version != args.expected_version:
        errors.append(
            f"workspace version mismatch: expected {args.expected_version}, got {version or 'none'}"
        )

    workspace_names: list[str] = []
    # Resolve the glob below the requested root rather than the process cwd.
    manifest_paths = [Path("Cargo.toml"), *sorted((root / "crates").glob("*/Cargo.toml"))]
    normalized_manifest_paths: list[Path] = []
    for path in manifest_paths:
        normalized_manifest_paths.append(path if path == Path("Cargo.toml") else path.relative_to(root))

    for relative_path in normalized_manifest_paths:
        relative = relative_path.as_posix()
        manifest = cargo_toml if relative == "Cargo.toml" else read_text(root, relative, errors)
        name = section_value(manifest, "package", "name")
        if name is None:
            errors.append(f"{relative} is missing package.name")
            continue
        workspace_names.append(name)
        if section_bool(manifest, "package", "version.workspace") is not True:
            errors.append(f"{relative} must inherit package.version from the workspace")

    lock_text = read_text(root, "Cargo.lock", errors)
    lock_packages = parse_lock_packages(lock_text)
    for name in workspace_names:
        local_matches = [
            package
            for package in lock_packages
            if package.get("name") == name and "source" not in package
        ]
        if len(local_matches) != 1:
            errors.append(
                f"Cargo.lock must contain exactly one local package named {name}; "
                f"found {len(local_matches)}"
            )
            continue
        lock_version = local_matches[0].get("version")
        if lock_version != version:
            errors.append(
                f"Cargo.lock version mismatch for {name}: expected {version}, "
                f"got {lock_version or 'none'}"
            )

    changelog = read_text(root, "CHANGELOG.md", errors)
    release_match = re.search(
        r"(?m)^## \[([^]]+)] - ([0-9]{4}-[0-9]{2}-[0-9]{2})\s*$", changelog
    )
    release_version = release_match.group(1) if release_match else None
    release_date = release_match.group(2) if release_match else None
    if release_match is None:
        errors.append("CHANGELOG.md has no released version heading")
    else:
        if release_version != version:
            errors.append(
                f"latest changelog version mismatch: expected {version}, got {release_version}"
            )
        validate_iso_date(release_date, "latest changelog date", errors)
        link_match = re.search(
            rf"(?m)^\[{re.escape(release_version)}]:\s+(\S+)\s*$", changelog
        )
        if link_match is None or f"v{release_version}" not in link_match.group(1):
            errors.append(f"CHANGELOG.md is missing a v{release_version} release link")

    for relative in MAN_PAGES:
        man_page = read_text(root, relative, errors)
        header = re.match(
            r'^\.TH\s+\S+\s+\d+\s+"([^"]+)"\s+"notm ([^"]+)"', man_page
        )
        if header is None:
            errors.append(f"{relative} has no versioned .TH header")
            continue
        validate_iso_date(header.group(1), f"{relative} revision date", errors)
        if header.group(2) != version:
            errors.append(
                f"{relative} version mismatch: expected {version}, got {header.group(2)}"
            )

    metainfo_relative = f"packaging/{APP_ID}.metainfo.xml"
    metainfo_text = read_text(root, metainfo_relative, errors)
    metainfo_root: ET.Element | None = None
    if metainfo_text:
        try:
            metainfo_root = ET.fromstring(metainfo_text)
        except ET.ParseError as error:
            errors.append(f"cannot parse {metainfo_relative}: {error}")
    if metainfo_root is not None:
        checks = {
            "id": APP_ID,
            "launchable": f"{APP_ID}.desktop",
            "icon": APP_ID,
        }
        for element_name, expected in checks.items():
            actual = metainfo_root.findtext(element_name)
            if actual != expected:
                errors.append(
                    f"AppStream {element_name} mismatch: expected {expected}, got {actual or 'none'}"
                )
        binaries = [element.text for element in metainfo_root.findall("./provides/binary")]
        if "notm" not in binaries:
            errors.append("AppStream metadata does not provide the notm binary")
        media_types = [
            element.text for element in metainfo_root.findall("./provides/mediatype")
        ]
        if "x-scheme-handler/mailto" not in media_types:
            errors.append("AppStream metadata does not provide the mailto handler")
        releases = metainfo_root.findall("./releases/release")
        if not releases:
            errors.append("AppStream metadata has no release entries")
        else:
            appstream_version = releases[0].get("version")
            appstream_date = releases[0].get("date")
            if appstream_version != version:
                errors.append(
                    f"latest AppStream version mismatch: expected {version}, "
                    f"got {appstream_version or 'none'}"
                )
            if appstream_date != release_date:
                errors.append(
                    f"latest AppStream date mismatch: expected {release_date}, "
                    f"got {appstream_date or 'none'}"
                )

    desktop_relative = f"packaging/{APP_ID}.desktop"
    desktop = parse_desktop(read_text(root, desktop_relative, errors))
    desktop_expectations = {
        "Type": "Application",
        # Desktop Entry Version is the specification version, not notm's version.
        "Version": "1.0",
        "Name": "notm",
        "Exec": "notm launch %u",
        "TryExec": "notm",
        "Icon": APP_ID,
        "MimeType": "x-scheme-handler/mailto;",
    }
    for key, expected in desktop_expectations.items():
        actual = desktop.get(key)
        if actual != expected:
            errors.append(
                f"desktop {key} mismatch: expected {expected}, got {actual or 'none'}"
            )

    makefile = read_text(root, "Makefile", errors)
    make_id = re.search(r"(?m)^DESKTOP_ID\s*:=\s*(\S+)\s*$", makefile)
    if make_id is None or make_id.group(1) != APP_ID:
        errors.append(f"Makefile DESKTOP_ID must be {APP_ID}")

    app_manifest = read_text(root, "crates/notm-app/Cargo.toml", errors)
    binary_name = section_value(app_manifest, "bin", "name")
    if binary_name != "notm":
        errors.append(
            f"notm-app binary name mismatch: expected notm, got {binary_name or 'none'}"
        )

    attributes = read_text(root, ".gitattributes", errors)
    attribute_lines = {
        line.split("#", 1)[0].strip()
        for line in attributes.splitlines()
        if line.split("#", 1)[0].strip()
    }
    if ".git_archival.txt export-subst" not in attribute_lines:
        errors.append(".gitattributes must export-subst .git_archival.txt")

    archival = read_text(root, ".git_archival.txt", errors)
    commit_match = re.search(r"(?m)^commit=(.+)$", archival)
    date_match = re.search(r"(?m)^commit-date=(.+)$", archival)
    source_commit = commit_match.group(1).strip() if commit_match else None
    source_date = date_match.group(1).strip() if date_match else None
    if source_commit is None:
        errors.append(".git_archival.txt is missing commit provenance")
    elif source_commit == COMMIT_PLACEHOLDER:
        if args.require_archive_provenance or args.expected_source_commit:
            errors.append("source archive commit placeholder was not expanded")
    elif not COMMIT_RE.fullmatch(source_commit):
        errors.append(f"source archive commit is invalid: {source_commit}")
    elif args.expected_source_commit and source_commit != args.expected_source_commit:
        errors.append(
            f"source archive commit mismatch: expected {args.expected_source_commit}, "
            f"got {source_commit}"
        )
    if source_date is None:
        errors.append(".git_archival.txt is missing commit-date provenance")
    elif source_date == DATE_PLACEHOLDER:
        if args.require_archive_provenance:
            errors.append("source archive commit-date placeholder was not expanded")
    else:
        validate_iso_datetime(source_date, "source archive commit date", errors)

    return version, errors


def main() -> int:
    args = parse_args()
    root = Path(args.source_root).resolve()
    if not root.is_dir():
        print(f"source root is not a directory: {root}", file=sys.stderr)
        return 2
    if args.expected_source_commit and not COMMIT_RE.fullmatch(args.expected_source_commit):
        print(
            f"expected source commit is not a lowercase 40-hex object ID: "
            f"{args.expected_source_commit}",
            file=sys.stderr,
        )
        return 2

    version, errors = verify(root, args)
    if errors:
        print("release metadata verification failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    if args.print_version:
        print(version)
    else:
        print(f"release metadata ok: {version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
