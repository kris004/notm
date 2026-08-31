#!/usr/bin/env python3
"""Regression checks for Linux distribution-harness helper contracts."""

from __future__ import annotations

import os
from pathlib import Path

from distribution_e2e import (
    E2EFailure,
    Harness,
    LINUX_UNIX_SOCKET_PATH_MAX,
    harness_socket_path,
)


class AttachmentHarness(Harness):
    def __init__(self, statuses: list[dict[str, object]]) -> None:
        super().__init__(Path("/unused"), "unused")
        self.statuses = iter(statuses)
        self.requests = 0

    def request(  # type: ignore[override]
        self,
        command: str,
        args: dict[str, object] | None = None,
        *,
        require_ok: bool = True,
    ) -> dict[str, object]:
        del args, require_ok
        if command != "attachment_io_status":
            raise AssertionError(f"unexpected harness command: {command}")
        self.requests += 1
        return next(self.statuses)


def assert_attachment_failure(
    harness: AttachmentHarness,
    started: dict[str, object],
    expected_message: str,
    *,
    require_path: bool = True,
    expected_action: str | None = None,
) -> None:
    try:
        harness.wait_for_attachment_completion(
            started,
            "rejected attachment completion",
            require_path=require_path,
            expected_action=expected_action,
            timeout=0.1,
        )
    except E2EFailure as error:
        if expected_message not in str(error):
            raise AssertionError(
                f"unexpected attachment completion error: {error}"
            ) from error
    else:
        raise AssertionError(f"accepted attachment completion: {started!r}")


def check_attachment_completion_compatibility() -> None:
    legacy = AttachmentHarness([])
    legacy_result = legacy.wait_for_attachment_completion(
        {"ok": True, "path": "/legacy/path"}, "legacy save", timeout=0.1
    )
    if legacy_result["path"] != "/legacy/path" or legacy.requests != 0:
        raise AssertionError(f"legacy completion was not accepted: {legacy_result}")

    asynchronous = AttachmentHarness(
        [
            {"ok": True, "busy": True, "last_completion": None},
            {
                "ok": True,
                "busy": False,
                "last_completion": {
                    "generation": 4,
                    "request_id": 8,
                    "action": "save_to_directory",
                    "applied": True,
                    "path": "/async/path",
                    "error": None,
                },
            },
        ]
    )
    async_result = asynchronous.wait_for_attachment_completion(
        {"ok": True, "pending": True, "generation": 4, "request_id": 8},
        "async save",
        expected_action="save_to_directory",
        timeout=0.5,
    )
    if async_result["path"] != "/async/path" or asynchronous.requests != 2:
        raise AssertionError(f"async completion was not polled: {async_result}")

    busy_with_matching_completion = AttachmentHarness(
        [
            {
                "ok": True,
                "busy": True,
                "last_completion": {
                    "generation": 5,
                    "request_id": 9,
                    "action": "prepare_open",
                    "applied": True,
                    "path": "/private/open",
                    "error": None,
                },
            }
        ]
    )
    busy_result = busy_with_matching_completion.wait_for_attachment_completion(
        {"ok": True, "pending": True, "generation": 5, "request_id": 9},
        "async open with stale work remaining",
        expected_action="prepare_open",
        timeout=0.1,
    )
    if busy_result["path"] != "/private/open":
        raise AssertionError(f"matching busy completion was not accepted: {busy_result}")

    chooser_save = AttachmentHarness(
        [
            {
                "ok": True,
                "busy": False,
                "last_completion": {
                    "generation": 7,
                    "request_id": 11,
                    "action": "save_to_target",
                    "applied": True,
                    "path": "/chooser/path",
                    "error": None,
                },
            }
        ]
    )
    chooser_result = chooser_save.wait_for_attachment_completion(
        {"ok": True, "pending": True, "generation": 7, "request_id": 11},
        "async chooser save",
        expected_action="save_to_target",
        timeout=0.1,
    )
    if chooser_result["path"] != "/chooser/path":
        raise AssertionError(f"chooser completion was not accepted: {chooser_result}")

    allowed_error = AttachmentHarness(
        [
            {
                "ok": True,
                "busy": False,
                "last_completion": {
                    "generation": 5,
                    "request_id": 9,
                    "action": "prepare_open",
                    "applied": True,
                    "path": "/private/open",
                    "error": "No application is registered for text/plain",
                },
            }
        ]
    )
    allowed_error.wait_for_attachment_completion(
        {"ok": True, "pending": True, "generation": 5, "request_id": 9},
        "async open",
        require_path=False,
        expected_action="prepare_open",
        allowed_errors=("No application is registered",),
        timeout=0.1,
    )

    missing_terminal = AttachmentHarness(
        [
            {
                "ok": True,
                "busy": False,
                "last_completion": {
                    "generation": 6,
                    "request_id": 10,
                    "action": "prepare_open",
                    "applied": True,
                    "path": "/private/open",
                    "error": "Unable to find terminal required for application",
                },
            }
        ]
    )
    missing_terminal.wait_for_attachment_completion(
        {"ok": True, "pending": True, "generation": 6, "request_id": 10},
        "async open without a terminal",
        require_path=False,
        expected_action="prepare_open",
        allowed_errors=("Unable to find terminal required for application",),
        timeout=0.1,
    )

    legacy_allowed_error = AttachmentHarness([])
    legacy_allowed_error.wait_for_attachment_completion(
        {"ok": False, "error": "not supported by legacy opener"},
        "legacy open",
        require_path=False,
        allowed_errors=("not supported",),
        timeout=0.1,
    )
    if legacy_allowed_error.requests != 0:
        raise AssertionError("legacy Open error unexpectedly polled attachment status")

    save_error = AttachmentHarness(
        [
            {
                "ok": True,
                "busy": False,
                "last_completion": {
                    "generation": 8,
                    "request_id": 12,
                    "action": "save_to_directory",
                    "applied": True,
                    "path": None,
                    "error": "fixture write failed",
                },
            }
        ]
    )
    assert_attachment_failure(
        save_error,
        {"ok": True, "pending": True, "generation": 8, "request_id": 12},
        "fixture write failed",
        expected_action="save_to_directory",
    )

    assert_attachment_failure(
        AttachmentHarness([]),
        {"ok": True, "pending": True, "generation": 9},
        "neither a path nor an asynchronous token",
    )
    assert_attachment_failure(
        AttachmentHarness([]),
        {"ok": True, "pending": True, "generation": True, "request_id": 12},
        "neither a path nor an asynchronous token",
    )

    wrong_token = AttachmentHarness(
        [
            {
                "ok": True,
                "busy": False,
                "last_completion": {
                    "generation": 10,
                    "request_id": 99,
                    "action": "save_to_directory",
                    "applied": True,
                    "path": "/wrong-token/path",
                    "error": None,
                },
            }
        ]
    )
    assert_attachment_failure(
        wrong_token,
        {"ok": True, "pending": True, "generation": 10, "request_id": 13},
        "wrong attachment token",
        expected_action="save_to_directory",
    )

    wrong_action = AttachmentHarness(
        [
            {
                "ok": True,
                "busy": False,
                "last_completion": {
                    "generation": 11,
                    "request_id": 14,
                    "action": "prepare_open",
                    "applied": True,
                    "path": "/wrong-action/path",
                    "error": None,
                },
            }
        ]
    )
    assert_attachment_failure(
        wrong_action,
        {"ok": True, "pending": True, "generation": 11, "request_id": 14},
        "wrong attachment action",
        expected_action="save_to_directory",
    )

    stale = AttachmentHarness(
        [
            {
                "ok": True,
                "busy": False,
                "last_completion": {
                    "generation": 6,
                    "request_id": 10,
                    "action": "save_to_target",
                    "applied": False,
                    "path": "/stale/path",
                    "error": None,
                },
            }
        ]
    )
    assert_attachment_failure(
        stale,
        {"ok": True, "pending": True, "generation": 6, "request_id": 10},
        "stale attachment request",
        expected_action="save_to_target",
    )


def assert_rejected(root: Path) -> None:
    candidate = root / "1"
    try:
        harness_socket_path(root, 1)
    except E2EFailure as error:
        if "exceeds the Linux AF_UNIX limit" not in str(error):
            raise AssertionError(f"unexpected path-limit error: {error}") from error
    else:
        raise AssertionError(f"accepted oversized socket path: {candidate}")


def main() -> int:
    accepted_root = Path("/") / ("a" * 104)
    accepted = harness_socket_path(accepted_root, 1)
    if len(os.fsencode(accepted)) != LINUX_UNIX_SOCKET_PATH_MAX:
        raise AssertionError(f"accepted boundary is not 107 bytes: {accepted}")

    assert_rejected(Path("/") / ("a" * 105))

    encoded_character_length = len(os.fsencode("é"))
    if encoded_character_length <= 1:
        raise AssertionError("filesystem encoding did not encode é as multiple bytes")
    overhead = len(os.fsencode(Path("/") / "1"))
    repetitions = (LINUX_UNIX_SOCKET_PATH_MAX - overhead) // encoded_character_length + 1
    multibyte_root = Path("/") / ("é" * repetitions)
    multibyte_candidate = multibyte_root / "1"
    if len(str(multibyte_candidate)) > LINUX_UNIX_SOCKET_PATH_MAX:
        raise AssertionError("multibyte regression path is also character-count oversized")
    assert_rejected(multibyte_root)

    check_attachment_completion_compatibility()

    print("Distribution E2E helper smoke passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
