#!/usr/bin/env python3
"""Exercise text-field keyboard safety in a private headless Sway session.

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
TOKEN = "notm-ui-text-focus-smoke"


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


def run_wtype(environment: dict[str, str], *arguments: str) -> None:
    command = ["wtype", *arguments]
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
            f"{' '.join(command)} failed with exit {result.returncode}: "
            f"{result.stderr.strip() or result.stdout.strip()}"
        )


def focus_app_window(environment: dict[str, str], app_pid: int) -> dict[str, Any]:
    def sway_tree() -> dict[str, Any]:
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

    def app_node(node: dict[str, Any]) -> dict[str, Any] | None:
        if node.get("pid") == app_pid and node.get("type") in {"con", "floating_con"}:
            return node
        for child_group in ("nodes", "floating_nodes"):
            children = node.get(child_group, [])
            if not isinstance(children, list):
                continue
            for child in children:
                if isinstance(child, dict):
                    found = app_node(child)
                    if found is not None:
                        return found
        return None

    wait_until(
        f"notm pid {app_pid} to map a Sway window",
        lambda: app_node(sway_tree()),
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
        node = app_node(sway_tree())
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
    response_state = response.get("state")
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


def exercise_ui(environment: dict[str, str], harness: Harness, app_pid: int) -> None:
    print("[ui-text-focus] fixture app and test harness are ready", flush=True)
    assert_target_tags_unchanged(harness, "fixture baseline")

    # Programmatic focus is also used by normal-mode pane traversal.  Exercise
    # the complete modal path in one virtual-keyboard session: destructive
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
    run_wtype(
        environment,
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
        exercise_ui(environment, harness, app_process.pid)
        print("[ui-text-focus] PASS", flush=True)
        return 0
    except BaseException as error:
        print(f"[ui-text-focus] FAIL: {error}", file=sys.stderr, flush=True)
        print(f"--- notm log ({app_log}) ---\n{log_tail(app_log)}", file=sys.stderr)
        print(f"--- sway log ({sway_log}) ---\n{log_tail(sway_log)}", file=sys.stderr)
        return 1
    finally:
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
