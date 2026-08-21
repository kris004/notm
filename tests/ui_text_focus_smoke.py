#!/usr/bin/env python3
"""Exercise message navigation, text-field, and tag-editor keys in headless Sway.

This is an explicit UI smoke test rather than a Cargo test.  It drives the real
GTK window with ``wtype`` while the developer test harness is used only to set
up and inspect state.  No live mailbox or user desktop is touched.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
from typing import Any, Callable


TARGET_MESSAGE_ID = "unicode@fixture.test"
TARGET_QUERY = 'subject:"Unicode"'
TARGET_TAGS = ["inbox", "unread"]
THREAD_QUERY = 'subject:"Three message thread"'
THREAD_ROOT_ID = "thread-root-three-message@fixture.test"
THREAD_REPLY1_ID = "thread-reply1-three-message@fixture.test"
HTML_QUERY = "id:html-message@fixture.test"
SINGLE_TAG = "ui-single-tag-smoke"
MULTI_TAG = "ui-multi-tag-smoke"
TOKEN = "notm-ui-text-focus-smoke"
COMPOSER_BODY_MARKER = "physical composer shortcut smoke"


class SmokeFailure(RuntimeError):
    """A failure with enough context to act on from test output."""


def parse_args() -> argparse.Namespace:
    repo_root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary",
        type=Path,
        default=Path(os.environ.get("NOTM_TEST_BINARY", repo_root / "target/debug/notm")),
        help="notm binary to test (default: target/debug/notm)",
    )
    parser.add_argument("--in-dbus-session", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--work-dir", type=Path, help=argparse.SUPPRESS)
    return parser.parse_args()


def require_command(name: str) -> None:
    if shutil.which(name) is None:
        raise SmokeFailure(
            f"required command {name!r} was not found in PATH; "
            "install it before running this explicit UI smoke test"
        )


def terminate_process_group(process: subprocess.Popen[Any] | None) -> None:
    if process is None or process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=5)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        if process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)


def log_tail(path: Path, lines: int = 200) -> str:
    try:
        content = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError as error:
        return f"<could not read {path}: {error}>"
    return "\n".join(content[-lines:]) or "<empty>"


def wait_until(
    description: str,
    predicate: Callable[[], Any],
    *,
    timeout: float = 15.0,
    interval: float = 0.05,
) -> Any:
    deadline = time.monotonic() + timeout
    last_error: BaseException | None = None
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except (OSError, SmokeFailure) as error:
            last_error = error
        time.sleep(interval)
    detail = f"; last error: {last_error}" if last_error is not None else ""
    raise SmokeFailure(f"timed out waiting for {description}{detail}")


class Harness:
    def __init__(self, path: Path, token: str) -> None:
        self.path = path
        self.token = token

    def request(self, command: str, args: dict[str, Any] | None = None) -> dict[str, Any]:
        payload = {
            "token": self.token,
            "command": command,
            "args": args or {},
        }
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(20)
            client.connect(str(self.path))
            client.sendall(json.dumps(payload, separators=(",", ":")).encode() + b"\n")
            response_bytes = bytearray()
            while not response_bytes.endswith(b"\n"):
                chunk = client.recv(65536)
                if not chunk:
                    break
                response_bytes.extend(chunk)
        if not response_bytes:
            raise SmokeFailure(f"test harness returned no response for {command!r}")
        try:
            response = json.loads(response_bytes)
        except json.JSONDecodeError as error:
            raise SmokeFailure(
                f"test harness returned invalid JSON for {command!r}: "
                f"{response_bytes.decode(errors='replace')!r}"
            ) from error
        if not isinstance(response, dict):
            raise SmokeFailure(f"unexpected test harness response for {command!r}: {response!r}")
        if response.get("ok") is not True:
            raise SmokeFailure(f"test harness command {command!r} failed: {response!r}")
        return response

    def state(self) -> dict[str, Any]:
        response = self.request("app_state")
        state_value = response.get("state")
        if not isinstance(state_value, dict):
            raise SmokeFailure(f"app_state did not contain an object: {response!r}")
        return state_value

    def entry_state(self) -> dict[str, Any]:
        return self.request("entry_state")

    def wait_for_search(self) -> dict[str, Any]:
        def completed_state() -> dict[str, Any] | None:
            status = self.request("search_status")
            if status.get("loading") is True:
                return None
            error = status.get("error")
            if isinstance(error, str):
                raise SmokeFailure(f"search failed: {error}")
            return self.state()

        return wait_until("fixture search to complete", completed_state)


class WtypeDriver:
    """Keep each virtual keyboard connected so the headless seat stays active."""

    def __init__(self, environment: dict[str, str]) -> None:
        self.environment = environment
        self.sessions: list[subprocess.Popen[str]] = []

    def send(self, *arguments: str) -> None:
        command = ["wtype", *arguments, "-s", "600000"]
        process = subprocess.Popen(
            command,
            env=self.environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        self.sessions.append(process)
        time.sleep(0.05)
        if process.poll() is not None:
            stdout, stderr = process.communicate()
            raise SmokeFailure(
                f"{' '.join(command)} failed with exit {process.returncode}: "
                f"{stderr.strip() or stdout.strip()}"
            )

    def close(self) -> None:
        for process in reversed(self.sessions):
            terminate_process_group(process)


def sway_tree(environment: dict[str, str]) -> dict[str, Any]:
    result = subprocess.run(
        ["swaymsg", "-t", "get_tree"],
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=10,
        check=False,
    )
    if result.returncode != 0:
        raise SmokeFailure(
            "could not inspect the private Sway tree: "
            f"{result.stderr.strip() or result.stdout.strip()}"
        )
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise SmokeFailure(f"swaymsg returned invalid tree JSON: {result.stdout!r}") from error
    if not isinstance(value, dict):
        raise SmokeFailure(f"swaymsg returned an invalid tree: {value!r}")
    return value


def find_sway_node(
    node: dict[str, Any], predicate: Callable[[dict[str, Any]], bool]
) -> dict[str, Any] | None:
    if predicate(node):
        return node
    for child_group in ("nodes", "floating_nodes"):
        children = node.get(child_group, [])
        if not isinstance(children, list):
            continue
        for child in children:
            if isinstance(child, dict):
                found = find_sway_node(child, predicate)
                if found is not None:
                    return found
    return None


def sway_node_title(node: dict[str, Any]) -> str:
    name = node.get("name")
    if isinstance(name, str):
        return name
    properties = node.get("window_properties")
    if isinstance(properties, dict):
        title = properties.get("title")
        if isinstance(title, str):
            return title
    return ""


def focus_app_window(environment: dict[str, str], app_pid: int) -> dict[str, Any]:
    def app_node(node: dict[str, Any]) -> dict[str, Any] | None:
        return find_sway_node(
            node,
            lambda candidate: candidate.get("pid") == app_pid
            and candidate.get("type") in {"con", "floating_con"},
        )

    wait_until(
        f"notm pid {app_pid} to map a Sway window",
        lambda: app_node(sway_tree(environment)),
        timeout=15,
    )
    command = ["swaymsg", f'[pid="{app_pid}"]', "focus"]
    result = subprocess.run(
        command,
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=10,
        check=False,
    )
    if result.returncode != 0:
        raise SmokeFailure(
            f"could not focus the notm window through private Sway: "
            f"{result.stderr.strip() or result.stdout.strip()}"
        )
    try:
        responses = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise SmokeFailure(f"swaymsg returned invalid JSON: {result.stdout!r}") from error
    if not isinstance(responses, list) or not any(
        isinstance(response, dict) and response.get("success") is True
        for response in responses
    ):
        raise SmokeFailure(f"private Sway did not focus notm pid {app_pid}: {responses!r}")

    def focused_app_node() -> dict[str, Any] | None:
        node = app_node(sway_tree(environment))
        return node if node is not None and node.get("focused") is True else None

    return wait_until(
        f"notm pid {app_pid} to receive compositor focus",
        focused_app_node,
        timeout=5,
    )


def selected_target(state_value: dict[str, Any]) -> dict[str, Any] | None:
    selected = state_value.get("selected_message")
    if isinstance(selected, dict) and selected.get("message_id") == TARGET_MESSAGE_ID:
        return selected
    return None


def load_target(harness: Harness) -> dict[str, Any]:
    response = harness.request("run_search", {"query": TARGET_QUERY})
    if response.get("scheduled") is not True:
        raise SmokeFailure(f"fixture search was not scheduled: {response!r}")
    response_state = harness.wait_for_search()
    if isinstance(response_state, dict):
        message = selected_target(response_state)
        if message is not None:
            return message

    # Search normally selects the first result.  Keeping this explicit fallback
    # makes the smoke resilient to list-focus policy changes without weakening
    # the assertion about the selected fixture message.
    page = harness.request("thread_page_info")
    loaded_count = page.get("loaded")
    if not isinstance(loaded_count, int) or loaded_count < 1:
        raise SmokeFailure(
            f"fixture message query did not load a thread: {page!r}"
        )
    rows = harness.request("thread_list_rows").get("rows")
    if not isinstance(rows, list):
        raise SmokeFailure(f"thread_list_rows did not return a list: {rows!r}")
    target_index = next(
        (
            row.get("index")
            for row in rows
            if isinstance(row, dict) and row.get("subject") == "Unicode ☕ message"
        ),
        None,
    )
    if not isinstance(target_index, int):
        raise SmokeFailure(
            f"fixture query did not contain the exact Unicode message row: {rows!r}"
        )
    harness.request("select_thread_by_index", {"index": target_index})

    def loaded() -> dict[str, Any] | None:
        return selected_target(harness.state())

    return wait_until(f"fixture message {TARGET_MESSAGE_ID!r}", loaded, timeout=5)


def assert_target_tags_unchanged(harness: Harness, phase: str) -> None:
    message = load_target(harness)
    assert_message_tags(message, phase)


def assert_message_tags(message: dict[str, Any], phase: str) -> None:
    actual = sorted(message.get("tags", []))
    expected = sorted(TARGET_TAGS)
    if actual != expected:
        raise SmokeFailure(
            f"{phase}: {TARGET_MESSAGE_ID} tags changed; "
            f"expected {expected!r}, got {actual!r}"
        )


def assert_selected_target_tags_unchanged(harness: Harness, phase: str) -> dict[str, Any]:
    selected = harness.state().get("selected_message")
    if not isinstance(selected, dict) or selected.get("message_id") != TARGET_MESSAGE_ID:
        raise SmokeFailure(
            f"{phase}: composer shortcut changed the selected fixture message: {selected!r}"
        )
    assert_message_tags(selected, phase)
    return selected


def wait_for_target_tag(harness: Harness, tag: str, present: bool) -> dict[str, Any]:
    def expected_state() -> dict[str, Any] | None:
        message = selected_target(harness.state())
        if message is None:
            return None
        tags = message.get("tags", [])
        return message if isinstance(tags, list) and ((tag in tags) == present) else None

    expectation = "gain" if present else "lose"
    return wait_until(
        f"fixture message {TARGET_MESSAGE_ID!r} to {expectation} tag {tag!r}",
        expected_state,
        timeout=5,
    )


def wait_for_tag_editor(
    harness: Harness,
    *,
    phase: str,
    mode: str,
    single_visible: bool,
    multiple_visible: bool,
    focused_field: str | None = None,
    menu_visible: bool | None = None,
) -> dict[str, Any]:
    last_entry: dict[str, Any] = {}

    def expected_state() -> dict[str, Any] | None:
        entry = harness.entry_state()
        last_entry.clear()
        last_entry.update(entry)
        matches = (
            entry.get("input_mode") == mode
            and entry.get("single_tag_editor_visible") is single_visible
            and entry.get("tag_command_editor_visible") is multiple_visible
        )
        if focused_field is not None:
            matches = matches and entry.get(f"{focused_field}_has_focus") is True
        if menu_visible is not None:
            matches = matches and entry.get("tag_menu_visible") is menu_visible
        return entry if matches else None

    try:
        return wait_until(f"{phase} tag editor state", expected_state, timeout=5)
    except SmokeFailure as error:
        raise SmokeFailure(f"{error}; last entry state: {last_entry!r}") from error


def focus_selected_thread(harness: Harness) -> None:
    """Move GTK focus off any text entry without changing the selection."""
    response = harness.request("select_relative_thread", {"delta": 0})
    state_value = response.get("state")
    if not isinstance(state_value, dict) or selected_target(state_value) is None:
        raise SmokeFailure(f"could not focus the selected fixture thread: {response!r}")


def wait_for_tag_menu(harness: Harness) -> None:
    last_entry: dict[str, Any] = {}

    def menu_visible() -> dict[str, Any] | None:
        entry = harness.entry_state()
        last_entry.clear()
        last_entry.update(entry)
        return (
            entry
            if entry.get("input_mode") == "Normal"
            and entry.get("tag_menu_visible") is True
            else None
        )

    try:
        wait_until("T tag choice menu", menu_visible, timeout=5)
    except SmokeFailure as error:
        raise SmokeFailure(f"{error}; last entry state: {last_entry!r}") from error


def wait_for_selected_message(harness: Harness, message_id: str) -> dict[str, Any]:
    def selected() -> dict[str, Any] | None:
        message = harness.state().get("selected_message")
        return (
            message
            if isinstance(message, dict) and message.get("message_id") == message_id
            else None
        )

    return wait_until(f"selected message {message_id!r}", selected, timeout=5)


def exercise_message_navigation(
    environment: dict[str, str], driver: WtypeDriver, harness: Harness, app_pid: int
) -> None:
    response = harness.request("run_search", {"query": THREAD_QUERY})
    if response.get("scheduled") is not True:
        raise SmokeFailure(f"thread navigation search was not scheduled: {response!r}")
    state_value = harness.wait_for_search()
    threads = state_value.get("thread_list_items")
    if not isinstance(threads, list) or len(threads) != 1:
        raise SmokeFailure(f"thread navigation fixture was not unique: {state_value!r}")
    harness.request("select_thread_by_index", {"index": 0})
    harness.request("select_message_by_index", {"index": 0})
    wait_for_selected_message(harness, THREAD_ROOT_ID)
    focus_app_window(environment, app_pid)

    driver.send("-M", "shift", "-k", "j", "-m", "shift")
    wait_for_selected_message(harness, THREAD_REPLY1_ID)

    # Lowercase j remains the message-scroll binding and must not change which
    # message is selected.
    driver.send("-k", "j")
    time.sleep(0.4)
    wait_for_selected_message(harness, THREAD_REPLY1_ID)

    driver.send("-M", "shift", "-k", "k", "-m", "shift")
    wait_for_selected_message(harness, THREAD_ROOT_ID)
    driver.send("-M", "shift", "-k", "k", "-m", "shift")
    time.sleep(0.4)
    wait_for_selected_message(harness, THREAD_ROOT_ID)

    driver.send("-M", "shift", "-k", "m", "-m", "shift")
    menu_state = wait_until(
        "M current-message action menu",
        lambda: (
            state
            if (state := harness.request("message_tag_state")).get("menu_popup_visible")
            is True
            else None
        ),
        timeout=5,
    )
    expected_labels = {
        "archive_label": "Archive message (M a)",
        "read_label": "Mark message read (M u)",
        "flag_label": "Flag message (M f)",
        "trash_label": "Move message to trash (M t)",
        "spam_label": "Mark message as spam (M s)",
        "custom_apply_label": "Add tag (M T)",
    }
    for field, expected in expected_labels.items():
        if menu_state.get(field) != expected:
            raise SmokeFailure(
                f"M menu did not expose {expected!r}: {menu_state!r}"
            )

    def current_message_unread(expected: bool) -> dict[str, Any] | None:
        state = harness.request("message_tag_state")
        selected = state.get("selected_message")
        if not isinstance(selected, dict) or selected.get("message_id") != THREAD_ROOT_ID:
            return None
        tags = selected.get("tags")
        if not isinstance(tags, list) or (("unread" in tags) != expected):
            return None
        return state if state.get("menu_popup_visible") is False else None

    driver.send("-k", "u")
    wait_until("M u to mark only the current message read", lambda: current_message_unread(False))

    driver.send("-M", "shift", "-k", "m", "-m", "shift", "-k", "u")
    wait_until(
        "a second M u to restore the current message unread tag",
        lambda: current_message_unread(True),
    )

    driver.send("-M", "shift", "-k", "m", "-m", "shift", "-M", "shift", "-k", "t", "-m", "shift")

    def message_custom_tag_editor_ready() -> dict[str, Any] | None:
        entry = harness.entry_state()
        menu = harness.request("message_tag_state")
        return (
            entry
            if entry.get("input_mode") == "Insert"
            and entry.get("message_custom_tag_has_focus") is True
            and menu.get("menu_popup_visible") is True
            else None
        )

    wait_until("M T to focus the current-message custom-tag field", message_custom_tag_editor_ready)
    driver.send("-k", "Escape", "-k", "Escape")
    print(
        "[ui-message-navigation] J/K navigation and M a/u/f/t/s/T actions passed",
        flush=True,
    )


def exercise_viewport_scroll_shortcuts(
    environment: dict[str, str], driver: WtypeDriver, harness: Harness, app_pid: int
) -> None:
    response = harness.request("run_search", {"query": "*"})
    if response.get("scheduled") is not True:
        raise SmokeFailure(f"message-list fixture search was not scheduled: {response!r}")
    state_value = harness.wait_for_search()
    threads = state_value.get("thread_list_items")
    if not isinstance(threads, list) or len(threads) < 8:
        raise SmokeFailure(f"message-list fixture had too few rows: {state_value!r}")
    harness.request("select_thread_by_index", {"index": 0})
    time.sleep(0.3)

    def list_viewport() -> dict[str, Any]:
        return harness.request("thread_selection_view_state")

    initial = wait_until(
        "scrollable message-list viewport",
        lambda: (
            viewport
            if isinstance((viewport := list_viewport()).get("scroll_value"), (int, float))
            and isinstance(viewport.get("scroll_upper"), (int, float))
            and isinstance(viewport.get("scroll_page_size"), (int, float))
            and viewport["scroll_upper"] > viewport["scroll_page_size"]
            and viewport.get("selected_abs") == 0
            else None
        ),
        timeout=10,
    )
    initial_y = initial["scroll_value"]
    selected_thread = harness.state().get("selected_thread", {}).get("thread_id")
    if not isinstance(selected_thread, str):
        raise SmokeFailure(f"message-list fixture had no selected thread: {initial!r}")

    def assert_selection_unchanged(phase: str) -> None:
        state = harness.state()
        thread_id = state.get("selected_thread", {}).get("thread_id")
        viewport = list_viewport()
        if thread_id != selected_thread or viewport.get("selected_abs") != 0:
            raise SmokeFailure(
                f"{phase} changed the selected message: state={state!r}, viewport={viewport!r}"
            )

    def exercise_from_pane(pane: str) -> None:
        entry = harness.entry_state()
        if entry.get("input_mode") != "Normal" or entry.get("active_pane") != pane:
            raise SmokeFailure(f"viewport fixture was not in {pane}: {entry!r}")
        before = list_viewport().get("scroll_value")
        if not isinstance(before, (int, float)):
            raise SmokeFailure(f"message-list viewport had no baseline: {list_viewport()!r}")
        focus_app_window(environment, app_pid)
        driver.send("-M", "ctrl", "-k", "e", "-m", "ctrl")
        down = wait_until(
            f"physical Ctrl+e to scroll the message list from {pane}",
            lambda: (
                viewport
                if isinstance(
                    (value := (viewport := list_viewport()).get("scroll_value")),
                    (int, float),
                )
                and value > before
                else None
            ),
            timeout=5,
        )
        down_y = down["scroll_value"]
        assert_selection_unchanged(f"Ctrl+e from {pane}")
        driver.send("-M", "ctrl", "-k", "y", "-m", "ctrl")
        wait_until(
            f"physical Ctrl+y to scroll the message list from {pane}",
            lambda: (
                viewport
                if isinstance(
                    (value := (viewport := list_viewport()).get("scroll_value")),
                    (int, float),
                )
                and value < down_y
                else None
            ),
            timeout=5,
        )
        assert_selection_unchanged(f"Ctrl+y from {pane}")

    # Search completion leaves the message list active.
    exercise_from_pane("Threads")
    driver.send("-k", "l")
    wait_until(
        "message pane before global message-list viewport shortcut",
        lambda: (
            entry
            if (entry := harness.entry_state()).get("active_pane") == "Message"
            else None
        ),
        timeout=5,
    )
    exercise_from_pane("Message")
    final_y = list_viewport().get("scroll_value")
    if not isinstance(final_y, (int, float)) or final_y > initial_y + 1.0:
        raise SmokeFailure(
            f"Ctrl+y did not restore the message-list viewport: {list_viewport()!r}"
        )
    print(
        "[ui-viewport-scroll] physical Ctrl+e/Ctrl+y message-list scrolling passed",
        flush=True,
    )


def exercise_link_hints(
    environment: dict[str, str], driver: WtypeDriver, harness: Harness, app_pid: int
) -> None:
    response = harness.request("run_search", {"query": HTML_QUERY})
    if response.get("scheduled") is not True:
        raise SmokeFailure(f"HTML fixture search was not scheduled: {response!r}")
    state_value = harness.wait_for_search()
    threads = state_value.get("thread_list_items")
    if not isinstance(threads, list) or len(threads) != 1:
        raise SmokeFailure(f"HTML link fixture was not unique: {state_value!r}")
    harness.request("select_thread_by_index", {"index": 0})
    harness.request("select_relative_thread", {"delta": 0})
    focus_app_window(environment, app_pid)
    driver.send("-k", "l")
    wait_until(
        "message pane focus before link hints",
        lambda: (
            state
            if (state := harness.state()).get("active_pane") == "Message"
            else None
        ),
        timeout=5,
    )
    before_tags = harness.state().get("selected_message", {}).get("tags")

    driver.send("-M", "shift", "-k", "f", "-m", "shift")

    last_hints: dict[str, Any] = {}

    def hints_routed() -> dict[str, Any] | None:
        response = harness.request("link_hint_state")
        last_hints.clear()
        last_hints.update(response)
        hints = response.get("link_hints")
        return (
            response
            if isinstance(hints, dict)
            and response.get("html_visible") is True
            and (
                (
                    hints.get("active") is True
                    and hints.get("candidate_count") == 2
                    and hints.get("overlay_count") == 2
                )
                or (
                    hints.get("phase") == "idle"
                    and response.get("status_text")
                    == "No visible links in this HTML message"
                )
            )
            else None
        )

    try:
        routed = wait_until("F link-hint routing", hints_routed, timeout=10)
    except SmokeFailure as error:
        raise SmokeFailure(f"{error}; last link-hint state: {last_hints!r}") from error
    after_tags = harness.state().get("selected_message", {}).get("tags")
    if after_tags != before_tags:
        raise SmokeFailure(
            f"Shift+F reached the lowercase flag action: before={before_tags!r}, "
            f"after={after_tags!r}"
        )
    if routed.get("link_hints", {}).get("active") is True:
        baseline = harness.state()
        baseline_pane = baseline.get("active_pane")
        baseline_message_id = baseline.get("selected_message", {}).get("message_id")

        # Modal hint input must reach the link-hint controller before the
        # application's existing h/j navigation bindings.  Alt keeps the
        # label from being selected, so this checks precedence without opening
        # an external application from the self-contained smoke environment.
        for key in ("h", "j"):
            driver.send("-M", "alt", "-k", key, "-m", "alt")

            def conflicting_binding_suppressed() -> dict[str, Any] | None:
                hints_state = harness.request("link_hint_state")
                app_state = harness.state()
                hints = hints_state.get("link_hints")
                selected = app_state.get("selected_message")
                return (
                    app_state
                    if isinstance(hints, dict)
                    and hints.get("active") is True
                    and app_state.get("active_pane") == baseline_pane
                    and isinstance(selected, dict)
                    and selected.get("message_id") == baseline_message_id
                    and hints_state.get("status_text")
                    == "Link hints: type a displayed letter, Backspace, or Esc"
                    else None
                )

            wait_until(
                f"link hints to override the existing {key.upper()} binding",
                conflicting_binding_suppressed,
                timeout=5,
            )

        driver.send("-k", "Escape")

        def hints_cancelled() -> dict[str, Any] | None:
            response = harness.request("link_hint_state")
            hints = response.get("link_hints")
            return (
                response
                if isinstance(hints, dict)
                and hints.get("phase") == "idle"
                and hints.get("overlay_count") == 0
                else None
            )

        wait_until("Esc to cancel link hints", hints_cancelled, timeout=5)
        print(
            "[ui-link-hints] F labels visible links, overrides H/J, and Esc cancels",
            flush=True,
        )
    else:
        print(
            "[ui-link-hints] F reached link-hint mode; headless WebKit exposed no link geometry",
            flush=True,
        )


def exercise_composer_shortcuts(
    environment: dict[str, str], driver: WtypeDriver, harness: Harness, app_pid: int
) -> None:
    load_target(harness)
    focus_selected_thread(harness)
    focus_app_window(environment, app_pid)

    # Open the composer and reach its GtkSourceView through normal-mode keyboard
    # navigation rather than through the test harness or a synthetic router call.
    driver.send("-k", "c")

    def composer_opened() -> dict[str, Any] | None:
        entry = harness.entry_state()
        draft = harness.request("draft_list_state")
        return (
            entry
            if entry.get("input_mode") == "Normal"
            and entry.get("active_pane") == "Message"
            and draft.get("section", {}).get("mapped") is True
            else None
        )

    wait_until("physical c to open the composer", composer_opened, timeout=5)
    assert_selected_target_tags_unchanged(harness, "opened physical-key composer")

    # The focus order is From, To, Cc, Bcc, Subject, Body.  Repeated physical
    # j navigation must therefore enter the Vim-backed body without a harness
    # focus command.  `i` then enters Vim insert mode before typing the marker.
    driver.send(
        "-k",
        "j",
        "-k",
        "j",
        "-k",
        "j",
        "-k",
        "j",
        "-k",
        "j",
    )
    wait_until(
        "normal-mode composer navigation to focus the body",
        lambda: (
            entry
            if (entry := harness.entry_state()).get("input_mode") == "Insert"
            and entry.get("active_pane") == "Message"
            else None
        ),
        timeout=5,
    )
    driver.send("-k", "i", "-d", "20", COMPOSER_BODY_MARKER)

    def body_marker_entered() -> dict[str, Any] | None:
        entry = harness.entry_state()
        fields = entry.get("compose_fields")
        if not isinstance(fields, dict):
            return None
        body = fields.get("body")
        return (
            entry
            if isinstance(body, str)
            and body.endswith(COMPOSER_BODY_MARKER)
            and entry.get("input_mode") == "Insert"
            else None
        )

    wait_until("physical typing in the Vim composer body", body_marker_entered, timeout=5)

    # The first Esc belongs to GtkSourceView's Vim context; only the second Esc
    # leaves notm's Insert mode.  This is the exact focus transition that
    # exposed the composer/global-shortcut conflict in the live application.
    driver.send("-k", "Escape")
    wait_until(
        "first Escape to leave Vim insert mode",
        lambda: (
            entry
            if (entry := harness.entry_state()).get("input_mode") == "Insert"
            and entry.get("status") == "Vim composer"
            else None
        ),
        timeout=5,
    )
    driver.send("-k", "Escape")
    wait_until(
        "second Escape to leave notm Insert mode",
        lambda: (
            entry
            if (entry := harness.entry_state()).get("input_mode") == "Normal"
            else None
        ),
        timeout=5,
    )

    tags_before = assert_selected_target_tags_unchanged(
        harness, "before physical composer actions"
    ).get("tags")

    # Physical lowercase-s plus Shift must save this composer, not apply the
    # global spam action to the selected message.
    driver.send("-M", "shift", "-k", "s", "-m", "shift")

    def saved_draft() -> dict[str, Any] | None:
        assert_selected_target_tags_unchanged(
            harness, "physical Shift+S while composer is visible"
        )
        draft = harness.request("draft_list_state")
        active = draft.get("active_draft")
        saved_fields = active.get("saved_fields") if isinstance(active, dict) else None
        path = active.get("path") if isinstance(active, dict) else None
        return (
            draft
            if isinstance(saved_fields, dict)
            and isinstance(saved_fields.get("body"), str)
            and saved_fields["body"].endswith(COMPOSER_BODY_MARKER)
            and isinstance(path, str)
            and Path(path).is_file()
            else None
        )

    saved = wait_until("physical Shift+S to save the composer", saved_draft, timeout=10)
    active_draft = saved.get("active_draft")
    if not isinstance(active_draft, dict) or not isinstance(active_draft.get("path"), str):
        raise SmokeFailure(f"saved draft did not expose its persisted path: {saved!r}")
    saved_path = Path(active_draft["path"])

    # Make the saved draft deliberately dirty so physical x has one exact,
    # inspectable result: the composer clear confirmation.  Reject it through
    # the harness to preserve this composer for the final Shift+A assertion.
    dirty_subject = "physical x confirmation smoke"
    dirtied = harness.request("compose_set_subject", {"value": dirty_subject})
    if dirtied.get("ok") is not True:
        raise SmokeFailure(f"could not dirty the saved composer before x: {dirtied!r}")
    driver.send("-k", "x")

    def composer_clear_confirmation() -> dict[str, Any] | None:
        assert_selected_target_tags_unchanged(
            harness, "physical x while composer is visible"
        )
        pending = harness.request("pending_confirmation")
        pending_action = pending.get("pending")
        if isinstance(pending_action, dict):
            if pending_action.get("kind") != "clear_composer":
                raise SmokeFailure(
                    f"physical x opened an unexpected confirmation: {pending!r}"
                )
            return pending
        return None

    clear_state = wait_until(
        "physical x to request clearing the dirty composer",
        composer_clear_confirmation,
        timeout=5,
    )
    pending = clear_state.get("pending")
    confirmation_id = pending.get("id") if isinstance(pending, dict) else None
    if not isinstance(confirmation_id, int):
        raise SmokeFailure(f"composer clear confirmation had no id: {clear_state!r}")
    rejected = harness.request(
        "respond_confirmation", {"response": "reject", "id": confirmation_id}
    )
    completion = rejected.get("last_completion")
    if (
        rejected.get("ok") is not True
        or not isinstance(completion, dict)
        or completion.get("accepted") is not False
        or completion.get("succeeded") is not True
    ):
        raise SmokeFailure(f"composer clear rejection failed: {rejected!r}")
    preserved = harness.request("draft_list_state")
    preserved_active = preserved.get("active_draft")
    preserved_fields = preserved.get("compose_fields")
    if (
        not isinstance(preserved_active, dict)
        or preserved_active.get("path") != str(saved_path)
        or not isinstance(preserved_fields, dict)
        or preserved_fields.get("subject") != dirty_subject
        or preserved.get("section", {}).get("mapped") is not True
    ):
        raise SmokeFailure(
            f"rejecting physical x did not preserve the dirty composer: {preserved!r}"
        )
    if not saved_path.is_file():
        raise SmokeFailure(f"physical x deleted the saved draft at {saved_path}")
    if (
        assert_selected_target_tags_unchanged(
            harness, "completed physical composer actions"
        ).get("tags")
        != tags_before
    ):
        raise SmokeFailure("composer actions changed the selected message tags")

    # Verify physical lowercase-a plus Shift last on the preserved composer.
    # Native portal choosers do not consistently honor synthetic Escape in a
    # nested compositor, so observe the real chooser and let the isolated
    # fixture-process teardown close it without selecting any host file.
    focus_app_window(environment, app_pid)
    if harness.entry_state().get("input_mode") == "Insert":
        # Returning focus to the Vim body after closing a modal re-enters the
        # application input layer.  Leave it again before asserting a Normal-
        # mode shortcut, just as a user would after returning to the editor.
        driver.send("-k", "Escape")
        wait_until(
            "notm Normal mode after rejecting the composer confirmation",
            lambda: (
                entry
                if (entry := harness.entry_state()).get("input_mode") == "Normal"
                else None
            ),
            timeout=5,
        )

    def attachment_chooser() -> dict[str, Any] | None:
        assert_selected_target_tags_unchanged(
            harness, "physical Shift+A while composer is visible"
        )
        return find_sway_node(
            sway_tree(environment),
            lambda node: node.get("type") in {"con", "floating_con"}
            and "add attachment" in sway_node_title(node).lower(),
        )

    driver.send("-M", "shift", "-k", "a", "-m", "shift")
    chooser = wait_until(
        "physical Shift+A to open the Add attachment chooser",
        attachment_chooser,
        timeout=10,
    )
    if "add attachment" not in sway_node_title(chooser).lower():
        raise SmokeFailure(f"unexpected attachment chooser node: {chooser!r}")
    if (
        assert_selected_target_tags_unchanged(
            harness, "observed physical attachment chooser"
        ).get("tags")
        != tags_before
    ):
        raise SmokeFailure("physical Shift+A changed the selected message tags")
    print(
        "[ui-composer-shortcuts] Vim Esc/Esc and physical S/x/A composer actions passed",
        flush=True,
    )


def exercise_indexed_draft_delete_shortcut(
    environment: dict[str, str], driver: WtypeDriver, harness: Harness, app_pid: int
) -> None:
    load_target(harness)
    focus_selected_thread(harness)
    focus_app_window(environment, app_pid)
    driver.send("-k", "c")
    wait_until(
        "composer message pane before g d",
        lambda: (
            entry
            if (entry := harness.entry_state()).get("active_pane") == "Message"
            and entry.get("input_mode") == "Normal"
            and harness.request("draft_list_state").get("section", {}).get("mapped")
            is True
            else None
        ),
        timeout=5,
    )
    focused = harness.request("focus_compose_field", {"field": "from"})
    if focused.get("ok") is not True:
        raise SmokeFailure(f"could not focus the composer header before g d: {focused!r}")
    focus_app_window(environment, app_pid)
    driver.send("-k", "g", "-k", "d")

    def draft_search_finished() -> dict[str, Any] | None:
        status = harness.request("search_status")
        if status.get("loading") is True:
            return None
        state_value = harness.state()
        return state_value if state_value.get("current_query") == "tag:draft" else None

    wait_until(
        "physical g d to open Drafts from the message pane",
        draft_search_finished,
        timeout=10,
    )

    def indexed_draft_opened() -> dict[str, Any] | None:
        state_value = harness.state()
        active = state_value.get("active_draft")
        return (
            state_value
            if isinstance(active, dict) and active.get("indexed") is True
            else None
        )

    opened = wait_until("fixture indexed draft to open", indexed_draft_opened, timeout=5)
    active = opened.get("active_draft")
    if not isinstance(active, dict):
        raise SmokeFailure(f"indexed draft had no active state: {opened!r}")
    path_value = active.get("path")
    message_id = active.get("message_id")
    if not isinstance(path_value, str) or not isinstance(message_id, str):
        raise SmokeFailure(f"indexed draft identity was incomplete: {active!r}")
    draft_path = Path(path_value)
    if not draft_path.is_file():
        raise SmokeFailure(f"indexed fixture draft is missing at {draft_path}")

    focus_app_window(environment, app_pid)
    entry = harness.entry_state()
    if entry.get("input_mode") == "Insert":
        driver.send("-k", "Escape")
        time.sleep(0.1)
        if harness.entry_state().get("input_mode") == "Insert":
            driver.send("-k", "Escape")
    wait_until(
        "indexed draft composer to enter Normal mode",
        lambda: (
            state
            if (state := harness.entry_state()).get("input_mode") == "Normal"
            else None
        ),
        timeout=5,
    )

    # Exercise the physical lowercase-d plus Shift path.  Accept through the
    # fixture harness so the check is independent of compositor focus moving
    # from the main window to GTK's modal surface.
    driver.send("-M", "shift", "-k", "d", "-m", "shift")

    def delete_confirmation() -> dict[str, Any] | None:
        pending = harness.request("pending_confirmation")
        action = pending.get("pending")
        if isinstance(action, dict):
            if action.get("kind") != "delete_active_draft":
                raise SmokeFailure(
                    f"physical Shift+D opened an unexpected confirmation: {pending!r}"
                )
            if action.get("visible") is not True:
                raise SmokeFailure(
                    f"physical Shift+D confirmation was not visible: {pending!r}"
                )
            return pending
        return None

    pending = wait_until(
        "physical Shift+D to request indexed draft deletion",
        delete_confirmation,
        timeout=5,
    )
    action = pending.get("pending")
    confirmation_id = action.get("id") if isinstance(action, dict) else None
    if not isinstance(confirmation_id, int):
        raise SmokeFailure(f"draft deletion confirmation had no id: {pending!r}")
    accepted = harness.request(
        "respond_confirmation", {"response": "accept", "id": confirmation_id}
    )
    completion = accepted.get("last_completion")
    if (
        not isinstance(completion, dict)
        or completion.get("accepted") is not True
        or completion.get("succeeded") is not True
    ):
        raise SmokeFailure(f"physical Shift+D deletion failed: {accepted!r}")

    def deletion_visible() -> dict[str, Any] | None:
        state_value = harness.state()
        rows = state_value.get("thread_list_items")
        if not isinstance(rows, list):
            return None
        retained = any(
            isinstance(row, dict) and row.get("thread_id") == message_id for row in rows
        )
        view = harness.request("message_view_text").get("text")
        if isinstance(view, str) and "Could not parse body" in view:
            raise SmokeFailure(f"physical Shift+D rendered a missing body: {view!r}")
        return state_value if not retained and not draft_path.exists() else None

    wait_until(
        "physical Shift+D to remove the indexed draft row and file",
        deletion_visible,
        timeout=10,
    )
    harness.wait_for_search()
    print(
        "[ui-composer-shortcuts] physical Shift+D deleted an indexed draft",
        flush=True,
    )


def exercise_ui(
    environment: dict[str, str], driver: WtypeDriver, harness: Harness, app_pid: int
) -> None:
    print("[ui-text-focus] fixture app and test harness are ready", flush=True)
    assert_target_tags_unchanged(harness, "fixture baseline")

    # Programmatic focus is also used by normal-mode pane traversal.  Exercise
    # the complete modal path in one virtual-keyboard sequence: destructive
    # a/t/s keys must be suppressed in Normal, a pending `g` sequence must not
    # survive Return into Insert, and text must reach the Entry only in Insert.
    harness.request("set_search_query", {"query": ""})
    time.sleep(0.6)
    harness.request("focus_search")
    normal_entry = wait_until(
        "normal-mode search entry focus",
        lambda: (
            entry
            if (entry := harness.entry_state()).get("input_mode") == "Normal"
            and entry.get("search_has_focus") is True
            else None
        ),
    )
    if normal_entry.get("search") != "":
        raise SmokeFailure(f"search entry was not cleared: {normal_entry!r}")
    focus_app_window(environment, app_pid)
    driver.send(
        "-d",
        "20",
        "ats",
        "-s",
        "40",
        "-k",
        "g",
        "-k",
        "Return",
        "-s",
        "40",
        "-k",
        "Escape",
        "-s",
        "40",
        "-k",
        "i",
        "-s",
        "40",
        "-d",
        "20",
        "atsX",
        "-s",
        "40",
        "-k",
        "Escape",
    )

    def modal_sequence_finished() -> dict[str, Any] | None:
        entry = harness.entry_state()
        return (
            entry
            if entry.get("input_mode") == "Normal"
            and entry.get("search") == "atsX"
            else None
        )

    wait_until("Normal/Insert search key sequence", modal_sequence_finished)
    assert_target_tags_unchanged(harness, "focused search key sequence")
    print(
        "[ui-text-focus] Normal actions were suppressed and Insert typing was safe",
        flush=True,
    )


def exercise_tag_editor(
    environment: dict[str, str], driver: WtypeDriver, harness: Harness, app_pid: int
) -> None:
    load_target(harness)
    focus_selected_thread(harness)
    focus_app_window(environment, app_pid)
    driver.send(
        # An unrelated second key closes the choice without opening an editor.
        "-M",
        "shift",
        "-k",
        "t",
        "-m",
        "shift",
        "-s",
        "800",
        "-k",
        "x",
        "-s",
        "1200",
        # Add and then remove one tag through the explicit single-tag editor.
        "-M",
        "shift",
        "-k",
        "t",
        "-m",
        "shift",
        "-s",
        "800",
        "-k",
        "t",
        "-s",
        "1200",
        "-d",
        "20",
        SINGLE_TAG,
        "-k",
        "Return",
        "-s",
        "1500",
        "-k",
        "Return",
        "-s",
        "1500",
        "-k",
        "Escape",
        "-s",
        "1200",
        # Add and remove one tag through two explicit multi-tag editor opens.
        "-M",
        "shift",
        "-k",
        "t",
        "-m",
        "shift",
        "-s",
        "800",
        "-k",
        "m",
        "-s",
        "1200",
        "-d",
        "20",
        f"+{MULTI_TAG}",
        "-k",
        "Return",
        "-s",
        "1500",
        "-M",
        "shift",
        "-k",
        "t",
        "-m",
        "shift",
        "-s",
        "800",
        "-k",
        "m",
        "-s",
        "1200",
        "-k",
        "minus",
        "-d",
        "20",
        MULTI_TAG,
        "-k",
        "Return",
    )

    # A failed second key must cancel the pending T sequence without opening an
    # editor, changing mode, or applying a tag.
    wait_for_tag_menu(harness)
    wait_for_tag_editor(
        harness,
        phase="unrelated T x",
        mode="Normal",
        single_visible=False,
        multiple_visible=False,
        menu_visible=False,
    )
    assert_target_tags_unchanged(harness, "unrelated T sequence")

    # T t opens the explicit add/remove editor.  Enter adds an absent tag,
    # updates the action to Remove, and a second Enter removes it again.
    single = wait_for_tag_editor(
        harness,
        phase="T t open",
        mode="Insert",
        single_visible=True,
        multiple_visible=False,
        focused_field="custom_tag",
    )
    if single.get("single_tag_apply_label") != "Add tag":
        raise SmokeFailure(f"single-tag editor did not start as Add: {single!r}")
    wait_for_target_tag(harness, SINGLE_TAG, True)

    def remove_action_ready() -> dict[str, Any] | None:
        entry = harness.entry_state()
        return (
            entry
            if entry.get("single_tag_apply_label") == "Remove tag"
            and entry.get("single_tag_editor_visible") is True
            and entry.get("input_mode") == "Insert"
            else None
        )

    wait_until("single-tag action to switch to Remove", remove_action_ready)
    wait_for_target_tag(harness, SINGLE_TAG, False)
    wait_for_tag_editor(
        harness,
        phase="single-tag Escape",
        mode="Normal",
        single_visible=False,
        multiple_visible=False,
    )

    # T m opens the separate multi-change editor.  Apply one tag, then reopen
    # the same explicit flow and remove it so the fixture finishes unchanged.
    wait_for_tag_editor(
        harness,
        phase="T m add open",
        mode="Insert",
        single_visible=False,
        multiple_visible=True,
        focused_field="tag_command",
    )
    wait_for_target_tag(harness, MULTI_TAG, True)
    wait_for_tag_editor(
        harness,
        phase="T m add apply",
        mode="Normal",
        single_visible=False,
        multiple_visible=False,
    )

    wait_for_tag_editor(
        harness,
        phase="T m remove open",
        mode="Insert",
        single_visible=False,
        multiple_visible=True,
        focused_field="tag_command",
    )
    wait_for_target_tag(harness, MULTI_TAG, False)
    wait_for_tag_editor(
        harness,
        phase="T m remove apply",
        mode="Normal",
        single_visible=False,
        multiple_visible=False,
    )
    assert_target_tags_unchanged(harness, "completed tag editor flows")
    print(
        "[ui-tag-editor] T x was safe and explicit T t/T m flows passed",
        flush=True,
    )


def socket_name(path: Path) -> str | None:
    try:
        return path.name if stat.S_ISSOCK(path.stat().st_mode) else None
    except OSError:
        return None


def run_inside_dbus(args: argparse.Namespace) -> int:
    if args.work_dir is None:
        raise SmokeFailure("internal error: --work-dir is required in the D-Bus session")
    work_dir = args.work_dir.resolve()
    runtime_dir = work_dir / "runtime"
    socket_path = work_dir / "notm-test-harness.sock"
    sway_log = work_dir / "sway.log"
    app_log = work_dir / "notm.log"
    sway_config = work_dir / "sway.conf"
    sway_config.write_text(
        "xwayland disable\n"
        "focus_follows_mouse no\n"
        "default_border pixel 1\n"
        "output * background #202020 solid_color\n",
        encoding="utf-8",
    )

    environment = os.environ.copy()
    environment.pop("DISPLAY", None)
    environment.pop("WAYLAND_DISPLAY", None)
    environment.pop("SWAYSOCK", None)
    environment.update(
        {
            "XDG_RUNTIME_DIR": str(runtime_dir),
            "XDG_CONFIG_HOME": str(work_dir / "config"),
            "XDG_CACHE_HOME": str(work_dir / "cache"),
            "XDG_DATA_HOME": str(work_dir / "data"),
            "HOME": str(work_dir / "home"),
            "WLR_BACKENDS": "headless",
            "WLR_HEADLESS_OUTPUTS": "1",
            "WLR_RENDERER": "pixman",
            "WLR_LIBINPUT_NO_DEVICES": "1",
            "GDK_BACKEND": "wayland",
            "NO_AT_BRIDGE": "1",
            "GSETTINGS_BACKEND": "memory",
            "GTK_USE_PORTAL": "0",
        }
    )
    for directory in (
        runtime_dir,
        work_dir / "config",
        work_dir / "cache",
        work_dir / "data",
        work_dir / "home",
    ):
        directory.mkdir(parents=True, exist_ok=True)
    runtime_dir.chmod(0o700)

    sway_process: subprocess.Popen[Any] | None = None
    app_process: subprocess.Popen[Any] | None = None
    driver: WtypeDriver | None = None
    try:
        with sway_log.open("wb") as sway_output:
            sway_process = subprocess.Popen(
                ["sway", "--config", str(sway_config)],
                env=environment,
                stdout=sway_output,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )

        def wayland_socket_ready() -> str | None:
            if sway_process is not None and sway_process.poll() is not None:
                raise SmokeFailure(
                    f"headless Sway exited with {sway_process.returncode}; "
                    f"log follows:\n{log_tail(sway_log)}"
                )
            for candidate in sorted(runtime_dir.glob("wayland-*")):
                name = socket_name(candidate)
                if name is not None:
                    return name
            return None

        wayland_display = wait_until("headless Sway Wayland socket", wayland_socket_ready)
        environment["WAYLAND_DISPLAY"] = wayland_display

        def sway_socket_ready() -> str | None:
            for candidate in sorted(runtime_dir.glob("sway-ipc.*.sock")):
                name = socket_name(candidate)
                if name is not None:
                    return str(candidate)
            return None

        environment["SWAYSOCK"] = wait_until("headless Sway IPC socket", sway_socket_ready)

        with app_log.open("wb") as app_output:
            app_process = subprocess.Popen(
                [
                    str(args.binary),
                    "launch",
                    "--fixture",
                    "--test-harness",
                    "--test-harness-socket",
                    str(socket_path),
                    "--test-harness-token",
                    TOKEN,
                ],
                cwd=Path(__file__).resolve().parents[1],
                env=environment,
                stdout=app_output,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )

        harness = Harness(socket_path, TOKEN)

        def harness_ready() -> bool:
            if app_process is not None and app_process.poll() is not None:
                raise SmokeFailure(
                    f"notm exited with {app_process.returncode}; "
                    f"log follows:\n{log_tail(app_log)}"
                )
            return socket_path.exists() and harness.request("health").get("state") == "running"

        wait_until("fixture test harness health", harness_ready, timeout=30)
        focus_app_window(environment, app_process.pid)
        driver = WtypeDriver(environment)
        # Establish the virtual keyboard before the first asserted shortcut.
        # A newly added headless seat can otherwise consume its first key while
        # Sway is assigning keyboard focus.
        driver.send("-k", "Escape")
        focus_app_window(environment, app_process.pid)
        exercise_message_navigation(environment, driver, harness, app_process.pid)
        exercise_viewport_scroll_shortcuts(
            environment, driver, harness, app_process.pid
        )
        exercise_link_hints(environment, driver, harness, app_process.pid)
        exercise_ui(environment, driver, harness, app_process.pid)
        exercise_tag_editor(environment, driver, harness, app_process.pid)
        exercise_indexed_draft_delete_shortcut(
            environment, driver, harness, app_process.pid
        )
        exercise_composer_shortcuts(environment, driver, harness, app_process.pid)
        print("[ui-text-focus] PASS", flush=True)
        return 0
    except BaseException as error:
        print(f"[ui-text-focus] FAIL: {error}", file=sys.stderr, flush=True)
        print(f"--- notm log ({app_log}) ---\n{log_tail(app_log)}", file=sys.stderr)
        print(f"--- sway log ({sway_log}) ---\n{log_tail(sway_log)}", file=sys.stderr)
        return 1
    finally:
        if driver is not None:
            driver.close()
        terminate_process_group(app_process)
        terminate_process_group(sway_process)


def run_outer(args: argparse.Namespace) -> int:
    for command in ("dbus-run-session", "sway", "swaymsg", "wtype"):
        require_command(command)
    binary = args.binary.expanduser().resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise SmokeFailure(
            f"notm test binary is missing or not executable: {binary}; "
            "build it with `cargo build -p notm-app` or pass --binary"
        )
    args.binary = binary

    with tempfile.TemporaryDirectory(prefix="notm-ui-text-focus-") as temp:
        temp_path = Path(temp)
        private_environment = os.environ.copy()
        private_environment.update(
            {
                "XDG_RUNTIME_DIR": str(temp_path / "runtime"),
                "XDG_CONFIG_HOME": str(temp_path / "config"),
                "XDG_CACHE_HOME": str(temp_path / "cache"),
                "XDG_DATA_HOME": str(temp_path / "data"),
                "HOME": str(temp_path / "home"),
                "NO_AT_BRIDGE": "1",
                "GSETTINGS_BACKEND": "memory",
                "GTK_USE_PORTAL": "0",
            }
        )
        for name in ("runtime", "config", "cache", "data", "home"):
            (temp_path / name).mkdir(mode=0o700)
        command = [
            "dbus-run-session",
            "--",
            sys.executable,
            str(Path(__file__).resolve()),
            "--in-dbus-session",
            "--work-dir",
            temp,
            "--binary",
            str(binary),
        ]
        result = subprocess.run(
            command,
            env=private_environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if result.stdout:
            print(result.stdout, end="")
        if result.returncode != 0 and result.stderr:
            print(result.stderr, end="", file=sys.stderr)
        return result.returncode


def main() -> int:
    args = parse_args()
    try:
        if args.in_dbus_session:
            return run_inside_dbus(args)
        return run_outer(args)
    except (OSError, SmokeFailure, subprocess.SubprocessError) as error:
        print(f"[ui-text-focus] FAIL: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
