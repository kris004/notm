#!/usr/bin/env python3
"""Exercise an installed notm distribution against disposable mail and SMTP.

The caller supplies the installed notm and Notmuch command paths.  This keeps
the test usable for an extracted native archive and for tiny wrappers around a
Flatpak installation.  All mail, configuration, drafts, sent copies, harness
sockets, and SMTP traffic are confined to a newly-created work directory.
"""

from __future__ import annotations

import argparse
import email.policy
import json
import os
import shutil
import signal
import socket
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
from collections.abc import Callable
from email.parser import BytesParser
from pathlib import Path
from typing import Any

STARTUP_TIMEOUT = 45.0
POLL_INTERVAL = 0.05
# Linux reserves one byte of sockaddr_un.sun_path for the terminating NUL.
LINUX_UNIX_SOCKET_PATH_MAX = 107
PLAIN_SUBJECT = "Distribution plain text"
PLAIN_MARKER = "notm distribution plain-text marker"
HTML_SUBJECT = "Distribution HTML"
HTML_MARKER = "notm distribution HTML marker"
ATTACHMENT_SUBJECT = "Distribution attachment"
ATTACHMENT_NAME = "distribution-note.txt"
ATTACHMENT_BYTES = b"notm distribution attachment payload\n"
DRAFT_SUBJECT = "Distribution restart draft"
SMTP_SUBJECT = "Distribution SMTP capture"
MAILTO_SUBJECT = "Distribution mailto routing"
MAILTO_BODY = "Opened through the installed mailto launch route."


class E2EFailure(RuntimeError):
    """A distribution failure with actionable context."""


def harness_socket_path(work_root: Path, run_number: int) -> Path:
    """Return a compact Linux pathname socket or fail before spawning notm."""
    path = work_root / str(run_number)
    path_length = len(os.fsencode(path))
    if path_length > LINUX_UNIX_SOCKET_PATH_MAX:
        raise E2EFailure(
            "test harness socket path exceeds the Linux AF_UNIX limit "
            f"({path_length} > {LINUX_UNIX_SOCKET_PATH_MAX} bytes): {path}"
        )
    return path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--notm", required=True, type=Path, help="installed notm executable or wrapper"
    )
    parser.add_argument(
        "--notmuch",
        required=True,
        type=Path,
        help="installed Notmuch executable or wrapper",
    )
    parser.add_argument(
        "--smtp-command",
        required=True,
        help="SMTP helper path as seen by notm (for example /app/bin/msmtp)",
    )
    parser.add_argument(
        "--work-root",
        type=Path,
        help="use this empty directory instead of creating a temporary one",
    )
    parser.add_argument(
        "--preserve-home",
        action="store_true",
        help=(
            "preserve an already-disposable HOME and XDG roots supplied by the "
            "caller; requires --work-root strictly beneath HOME"
        ),
    )
    parser.add_argument(
        "--require-display",
        action="store_true",
        help="fail instead of skipping when neither WAYLAND_DISPLAY nor DISPLAY is set",
    )
    parser.add_argument(
        "--exercise-portal-link",
        action="store_true",
        help=(
            "activate the fixture mailto HTML link through GIO; use only in an "
            "isolated desktop/MIME environment"
        ),
    )
    parser.add_argument(
        "--keep-work", action="store_true", help="preserve temporary evidence"
    )
    return parser.parse_args()


def wait_until(
    description: str,
    predicate: Callable[[], Any],
    *,
    timeout: float = STARTUP_TIMEOUT,
) -> Any:
    deadline = time.monotonic() + timeout
    last_error: BaseException | None = None
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except (OSError, E2EFailure) as error:
            last_error = error
        time.sleep(POLL_INTERVAL)
    detail = f"; last error: {last_error}" if last_error else ""
    raise E2EFailure(f"timed out waiting for {description}{detail}")


def terminate_process_group(process: subprocess.Popen[bytes] | None) -> None:
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


class Harness:
    def __init__(self, socket_path: Path, token: str) -> None:
        self.socket_path = socket_path
        self.token = token

    def request(
        self,
        command: str,
        args: dict[str, Any] | None = None,
        *,
        require_ok: bool = True,
    ) -> dict[str, Any]:
        payload = {"token": self.token, "command": command, "args": args or {}}
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(20)
            client.connect(str(self.socket_path))
            client.sendall(json.dumps(payload, separators=(",", ":")).encode() + b"\n")
            response_bytes = bytearray()
            while not response_bytes.endswith(b"\n"):
                chunk = client.recv(65536)
                if not chunk:
                    break
                response_bytes.extend(chunk)
        if not response_bytes:
            raise E2EFailure(f"no harness response for {command!r}")
        try:
            response = json.loads(response_bytes)
        except json.JSONDecodeError as error:
            raise E2EFailure(
                f"invalid harness JSON for {command!r}: {response_bytes!r}"
            ) from error
        if not isinstance(response, dict):
            raise E2EFailure(
                f"non-object harness response for {command!r}: {response!r}"
            )
        if require_ok and response.get("ok") is not True:
            raise E2EFailure(f"harness command {command!r} failed: {response!r}")
        return response

    def wait_for_search(self) -> dict[str, Any]:
        def settled() -> dict[str, Any] | None:
            status = self.request("search_status")
            if status.get("loading") is True or status.get("generation") == 0:
                return None
            if status.get("error") is not None:
                raise E2EFailure(f"search failed: {status!r}")
            state = self.request("app_state")
            if state.get("state", {}).get("search_loading") is True:
                return None
            return state

        return wait_until("search completion", settled)

    def select_subject(self, subject: str) -> dict[str, Any]:
        scheduled = self.request("run_search", {"query": f'subject:"{subject}"'})
        if scheduled.get("scheduled") is not True:
            raise E2EFailure(f"search was not scheduled: {scheduled!r}")
        self.wait_for_search()
        rows = self.request("thread_list_rows").get("rows")
        if not isinstance(rows, list):
            raise E2EFailure(f"thread rows were not a list: {rows!r}")
        index = next(
            (
                row.get("index")
                for row in rows
                if isinstance(row, dict) and row.get("subject") == subject
            ),
            None,
        )
        if not isinstance(index, int):
            raise E2EFailure(f"subject {subject!r} was absent from rows: {rows!r}")
        self.request("select_thread_by_index", {"index": index})

        def selected() -> dict[str, Any] | None:
            state = self.request("app_state")
            selected_message = state.get("state", {}).get("selected_message")
            if (
                isinstance(selected_message, dict)
                and selected_message.get("subject") == subject
            ):
                return state
            return None

        return wait_until(f"message {subject!r} selection", selected)

    def wait_for_send(self) -> dict[str, Any]:
        def completed() -> dict[str, Any] | None:
            state = self.request("app_state")
            app = state.get("state", {})
            if app.get("send_in_progress") is True:
                return None
            report = app.get("last_send_report")
            if not isinstance(report, dict):
                return None
            if report.get("accepted") is not True:
                raise E2EFailure(f"send was not accepted: {state!r}")
            return state

        return wait_until("SMTP send completion", completed)

    def wait_for_attachment_completion(
        self,
        started: dict[str, Any],
        description: str,
        *,
        require_path: bool = True,
        expected_action: str | None = None,
        allowed_errors: tuple[str, ...] = (),
        timeout: float = STARTUP_TIMEOUT,
    ) -> dict[str, Any]:
        """Accept legacy synchronous attachment replies or poll async I/O."""

        def validate(completion: dict[str, Any]) -> dict[str, Any]:
            error = completion.get("error")
            if isinstance(error, str) and error:
                if any(allowed in error for allowed in allowed_errors):
                    return completion
                raise E2EFailure(f"{description} failed: {completion!r}")
            if require_path and not isinstance(completion.get("path"), str):
                raise E2EFailure(f"{description} returned no path: {completion!r}")
            return completion

        if started.get("ok") is not True:
            return validate(started)
        if isinstance(started.get("path"), str):
            return validate(started)

        generation = started.get("generation")
        request_id = started.get("request_id")
        if (
            started.get("pending") is not True
            or type(generation) is not int
            or type(request_id) is not int
        ):
            raise E2EFailure(
                f"{description} returned neither a path nor an asynchronous token: "
                f"{started!r}"
            )

        def settled() -> dict[str, Any] | None:
            status = self.request("attachment_io_status")
            completion = status.get("last_completion")
            if (
                isinstance(completion, dict)
                and completion.get("generation") == generation
                and completion.get("request_id") == request_id
            ):
                return status
            if status.get("busy") is True:
                return None
            return status

        status = wait_until(description, settled, timeout=timeout)
        completion = status.get("last_completion")
        if not isinstance(completion, dict):
            raise E2EFailure(f"{description} completed without a result: {status!r}")
        if (
            completion.get("generation") != generation
            or completion.get("request_id") != request_id
        ):
            raise E2EFailure(
                f"{description} completed with the wrong attachment token: {status!r}"
            )
        if expected_action is not None and completion.get("action") != expected_action:
            raise E2EFailure(
                f"{description} completed with the wrong attachment action: {status!r}"
            )
        if completion.get("applied") is not True:
            raise E2EFailure(
                f"{description} completed as a stale attachment request: {status!r}"
            )
        return validate(completion)


class SMTPState:
    def __init__(self, capture_path: Path) -> None:
        self.capture_path = capture_path
        self.envelope_from: str | None = None
        self.recipients: list[str] = []
        self.event = threading.Event()


class SMTPHandler(socketserver.StreamRequestHandler):
    server: SMTPServer

    def handle(self) -> None:
        self.wfile.write(b"220 localhost notm distribution capture\r\n")
        in_data = False
        message = bytearray()
        while True:
            line = self.rfile.readline(1024 * 1024)
            if not line:
                return
            if in_data:
                if line in {b".\r\n", b".\n"}:
                    self.server.state.capture_path.write_bytes(bytes(message))
                    self.server.state.event.set()
                    self.wfile.write(b"250 2.0.0 captured\r\n")
                    in_data = False
                    continue
                if line.startswith(b".."):
                    line = line[1:]
                message.extend(line)
                continue

            command = line.decode("utf-8", errors="replace").rstrip("\r\n")
            verb, _, argument = command.partition(" ")
            verb = verb.upper()
            if verb in {"EHLO", "HELO"}:
                self.wfile.write(b"250-localhost\r\n250-8BITMIME\r\n250 SMTPUTF8\r\n")
            elif verb == "MAIL":
                self.server.state.envelope_from = argument
                self.wfile.write(b"250 2.1.0 sender ok\r\n")
            elif verb == "RCPT":
                self.server.state.recipients.append(argument)
                self.wfile.write(b"250 2.1.5 recipient ok\r\n")
            elif verb == "DATA":
                in_data = True
                self.wfile.write(b"354 end with <CRLF>.<CRLF>\r\n")
            elif verb == "RSET":
                message.clear()
                self.wfile.write(b"250 2.0.0 reset\r\n")
            elif verb == "QUIT":
                self.wfile.write(b"221 2.0.0 bye\r\n")
                return
            else:
                self.wfile.write(b"250 2.0.0 ok\r\n")


class SMTPServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(self, state: SMTPState) -> None:
        self.state = state
        super().__init__(("127.0.0.1", 0), SMTPHandler)


def make_maildir(path: Path) -> None:
    for child in ("cur", "new", "tmp"):
        (path / child).mkdir(parents=True, exist_ok=True)


def write_messages(inbox: Path) -> None:
    messages = {
        "plain": (
            "From: Alice <alice@example.test>\n"
            "To: Distribution User <distribution@example.test>\n"
            f"Subject: {PLAIN_SUBJECT}\n"
            "Message-ID: <distribution-plain@example.test>\n"
            "Date: Tue, 25 Aug 2026 12:00:00 +0000\n"
            "MIME-Version: 1.0\n"
            "Content-Type: text/plain; charset=UTF-8\n\n"
            f"{PLAIN_MARKER}\n"
        ).encode(),
        "html": (
            "From: Bob <bob@example.test>\n"
            "To: Distribution User <distribution@example.test>\n"
            f"Subject: {HTML_SUBJECT}\n"
            "Message-ID: <distribution-html@example.test>\n"
            "Date: Tue, 25 Aug 2026 12:01:00 +0000\n"
            "MIME-Version: 1.0\n"
            "Content-Type: multipart/alternative; boundary=notm-html\n\n"
            "--notm-html\nContent-Type: text/plain; charset=UTF-8\n\n"
            f"{HTML_MARKER}\n"
            "--notm-html\nContent-Type: text/html; charset=UTF-8\n\n"
            f"<html><body><p>{HTML_MARKER}</p>"
            '<a href="https://example.test/one">one</a>'
            '<a href="mailto:person@example.test">mail</a>'
            "</body></html>\n--notm-html--\n"
        ).encode(),
        "attachment": (
            "From: Carol <carol@example.test>\n"
            "To: Distribution User <distribution@example.test>\n"
            f"Subject: {ATTACHMENT_SUBJECT}\n"
            "Message-ID: <distribution-attachment@example.test>\n"
            "Date: Tue, 25 Aug 2026 12:02:00 +0000\n"
            "MIME-Version: 1.0\n"
            "Content-Type: multipart/mixed; boundary=notm-attachment\n\n"
            "--notm-attachment\nContent-Type: text/plain; charset=UTF-8\n\n"
            "A message with a distribution attachment.\n"
            "--notm-attachment\n"
            f"Content-Type: text/plain; name={ATTACHMENT_NAME}\n"
            "Content-Transfer-Encoding: base64\n"
            f"Content-Disposition: attachment; filename={ATTACHMENT_NAME}\n\n"
            "bm90bSBkaXN0cmlidXRpb24gYXR0YWNobWVudCBwYXlsb2FkCg==\n"
            "--notm-attachment--\n"
        ).encode(),
    }
    for name, content in messages.items():
        (inbox / "new" / f"17245872.{name}.notm:2,").write_bytes(content)


def toml_string(value: str | Path) -> str:
    return json.dumps(str(value), ensure_ascii=False)


def clean_environment(
    work_root: Path, *, preserve_home: bool = False
) -> dict[str, str]:
    environment = os.environ.copy()
    if preserve_home:
        home_value = environment.get("HOME")
        if not home_value:
            raise E2EFailure("--preserve-home requires HOME")
        home = Path(home_value)
        if not home.is_absolute() or home != home.resolve() or not home.is_dir():
            raise E2EFailure(
                "--preserve-home requires an existing, absolute, resolved HOME"
            )
        try:
            relative_work_root = work_root.relative_to(home)
        except ValueError as error:
            raise E2EFailure(
                "--preserve-home requires --work-root strictly beneath HOME"
            ) from error
        if not relative_work_root.parts:
            raise E2EFailure(
                "--preserve-home requires --work-root strictly beneath HOME"
            )

        for name in (
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
        ):
            value = environment.get(name)
            if not value:
                raise E2EFailure(f"--preserve-home requires {name}")
            path = Path(value)
            if not path.is_absolute() or path != path.resolve() or not path.is_dir():
                raise E2EFailure(
                    f"--preserve-home requires an existing, absolute, resolved {name}"
                )
            try:
                relative = path.relative_to(home)
            except ValueError as error:
                raise E2EFailure(
                    f"--preserve-home requires {name} beneath HOME"
                ) from error
            if not relative.parts:
                raise E2EFailure(
                    f"--preserve-home requires {name} strictly beneath HOME"
                )
    else:
        home = work_root / "home"
        environment.update(
            {
                "HOME": str(home),
                "XDG_CONFIG_HOME": str(home / ".config"),
                "XDG_CACHE_HOME": str(home / ".cache"),
                "XDG_DATA_HOME": str(home / ".local" / "share"),
                "XDG_STATE_HOME": str(home / ".local" / "state"),
            }
        )
    environment.update(
        {
            "GSETTINGS_BACKEND": "memory",
            "NO_AT_BRIDGE": "1",
            "NOTM_DISTRIBUTION_E2E_ROOT": str(work_root),
        }
    )
    for name in ("NOTMUCH_CONFIG", "NOTMUCH_DATABASE", "NOTMUCH_PROFILE", "MAILDIR"):
        environment.pop(name, None)
    for path_name in (
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
    ):
        Path(environment[path_name]).mkdir(parents=True, exist_ok=True)
    return environment


def run_checked(
    command: list[str], environment: dict[str, str], **kwargs: Any
) -> subprocess.CompletedProcess[Any]:
    result = subprocess.run(
        command,
        env=environment,
        text=True,
        capture_output=True,
        check=False,
        **kwargs,
    )
    if result.returncode != 0:
        raise E2EFailure(
            f"command failed ({result.returncode}): {command!r}\n"
            f"--- stdout ---\n{result.stdout}\n--- stderr ---\n{result.stderr}"
        )
    return result


class AppProcess:
    def __init__(
        self,
        notm: Path,
        config_path: Path | None,
        work_root: Path,
        environment: dict[str, str],
        run_number: int,
        *,
        fixture: bool = False,
        launch_target: str | None = None,
    ) -> None:
        self.log_path = work_root / f"notm-run-{run_number}.log"
        # Keep this deliberately compact. Flatpak maps the work directory below
        # ~/.var/app/<app-id>, so descriptive socket names can exceed Linux's
        # 107-byte pathname limit even when the disposable root itself is short.
        self.socket_path = harness_socket_path(work_root, run_number)
        self.token = f"notm-distribution-e2e-{os.getpid()}-{run_number}"
        if self.socket_path.exists() or self.socket_path.is_symlink():
            self.socket_path.unlink()
        command = [str(notm)]
        if config_path is not None:
            command.extend(["--config", str(config_path)])
        command.append("launch")
        if launch_target is not None:
            command.append(launch_target)
        command.extend(
            [
                "--test-harness",
                "--test-harness-socket",
                str(self.socket_path),
                "--test-harness-token",
                self.token,
            ]
        )
        if fixture:
            command.append("--fixture")
        log = self.log_path.open("xb")
        self.process = subprocess.Popen(
            command,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        log.close()
        self.harness = Harness(self.socket_path, self.token)

        def connected() -> bool:
            status = self.process.poll()
            if status is not None:
                raise E2EFailure(
                    f"notm exited during startup with {status}\n{self.logs()}"
                )
            if not self.socket_path.exists():
                return False
            return self.harness.request("health").get("state") == "running"

        wait_until("notm test harness", connected)

    def logs(self) -> str:
        try:
            return self.log_path.read_text(encoding="utf-8", errors="replace")
        except OSError as error:
            return f"<cannot read {self.log_path}: {error}>"

    def close(self) -> None:
        if self.process.poll() is not None:
            return
        self.harness.request("close_main_window")
        try:
            status = self.process.wait(timeout=1)
        except subprocess.TimeoutExpired:
            # A dirty fixture composer presents the real confirmation dialog.
            # The live mailto launch is cleared before close, so confirmation
            # control remains confined to fixture policy.
            try:
                pending_response = self.harness.request(
                    "pending_confirmation", require_ok=False
                )
            except (E2EFailure, OSError):
                pending_response = {}
            pending = pending_response.get("pending")
            if isinstance(pending, dict) and isinstance(pending.get("id"), int):
                accepted = self.harness.request(
                    "respond_confirmation",
                    {"id": pending["id"], "response": "accept"},
                )
                if accepted.get("ok") is not True:
                    raise E2EFailure(
                        f"could not accept close confirmation: {accepted!r}"
                    )
            try:
                status = self.process.wait(timeout=8)
            except subprocess.TimeoutExpired as error:
                raise E2EFailure(
                    f"notm did not exit after close\n{self.logs()}"
                ) from error
        if status != 0:
            raise E2EFailure(f"notm exited with {status}\n{self.logs()}")

    def abort(self) -> None:
        terminate_process_group(self.process)


def configure_mail(
    work_root: Path,
    environment: dict[str, str],
    notmuch: Path,
    smtp_command: str,
    smtp_port: int,
) -> tuple[Path, Path, Path, Path]:
    mail_root = work_root / "home" / "Mail"
    inbox = mail_root / "Inbox"
    drafts = mail_root / "Drafts"
    sent = mail_root / "Sent"
    for path in (inbox, drafts, sent):
        make_maildir(path)
    write_messages(inbox)

    config_dir = work_root / "config"
    config_dir.mkdir(parents=True, exist_ok=True)
    notmuch_config = config_dir / "notmuch-config"
    notmuch_config.write_text(
        "[database]\n"
        f"path={mail_root}\n"
        "[user]\n"
        "name=Distribution User\n"
        "primary_email=distribution@example.test\n"
        "other_email=alias@example.test\n"
        "[new]\n"
        "tags=inbox;unread\n"
        "ignore=\n"
        "[search]\n"
        "exclude_tags=deleted;spam\n"
        "[maildir]\n"
        "synchronize_flags=true\n",
        encoding="utf-8",
    )
    index_environment = environment.copy()
    index_environment["NOTMUCH_CONFIG"] = str(notmuch_config)
    run_checked([str(notmuch), "new"], index_environment)
    count = run_checked(
        [str(notmuch), "count", "tag:inbox"], index_environment
    ).stdout.strip()
    if count != "3":
        raise E2EFailure(f"expected three indexed inbox messages, got {count!r}")

    app_config = config_dir / "notm.toml"
    smtp_args = [
        "--host=127.0.0.1",
        f"--port={smtp_port}",
        "--tls=off",
        "--auth=off",
        "--from=distribution@example.test",
        "--read-recipients",
    ]
    app_config.write_text(
        "[notmuch]\n"
        f"database_path = {toml_string(mail_root)}\n"
        f"config_path = {toml_string(notmuch_config)}\n"
        'default_query = "tag:inbox"\n\n'
        "[identity]\n"
        'name = "Distribution User"\n'
        'primary_email = "distribution@example.test"\n\n'
        "[send]\n"
        "enabled = true\n"
        'transport = "external"\n'
        f"command = {toml_string(smtp_command)}\n"
        f"args = [{', '.join(toml_string(value) for value in smtp_args)}]\n"
        'mode = "stdin_rfc5322"\n'
        "timeout_seconds = 10\n"
        "save_sent = true\n"
        f"sent_maildir = {toml_string(sent)}\n"
        "index_sent_after_send = true\n\n"
        "[drafts]\n"
        "save_maildir = true\n"
        f"maildir = {toml_string(drafts)}\n"
        'tags = ["draft"]\n'
        "index_after_save = true\n\n"
        "[automation]\n"
        "allow_live_send_test = true\n"
        "allow_live_tag_test = true\n",
        encoding="utf-8",
    )
    return app_config, notmuch_config, drafts, sent


def exercise_first_run(app: AppProcess, work_root: Path) -> tuple[str, Path]:
    harness = app.harness
    harness.wait_for_search()

    harness.select_subject(PLAIN_SUBJECT)
    harness.request("show_rendered_thread")
    text = harness.request("message_view_text").get("text")
    if not isinstance(text, str) or PLAIN_MARKER not in text:
        raise E2EFailure(f"plain-text marker was not rendered: {text!r}")

    harness.select_subject(HTML_SUBJECT)
    shown = harness.request("show_visual_html")
    if shown.get("html_view", {}).get("has_html") is not True:
        raise E2EFailure(f"HTML message was not detected: {shown!r}")

    def html_visible() -> dict[str, Any] | None:
        state = harness.request("html_view_state")
        return state if state.get("html_visible") is True else None

    html_state = wait_until("WebKitGTK HTML view", html_visible)
    if not isinstance(html_state.get("html_bytes"), int) or html_state[
        "html_bytes"
    ] < len(HTML_MARKER):
        raise E2EFailure(f"HTML body was unexpectedly empty: {html_state!r}")

    harness.select_subject(ATTACHMENT_SUBJECT)
    attachments = harness.request("attachment_list_items").get("attachments")
    if not isinstance(attachments, list) or len(attachments) != 1:
        raise E2EFailure(f"attachment list was unexpected: {attachments!r}")
    if attachments[0].get("filename") != ATTACHMENT_NAME:
        raise E2EFailure(f"attachment filename was unexpected: {attachments!r}")
    downloads = work_root / "downloads"
    downloads.mkdir()
    saved = harness.request(
        "save_selected_attachment", {"index": 0, "dir": str(downloads)}
    )
    saved = harness.wait_for_attachment_completion(
        saved, "attachment save", expected_action="save_to_directory"
    )
    saved_path = Path(saved["path"])
    if saved_path.read_bytes() != ATTACHMENT_BYTES:
        raise E2EFailure(f"saved attachment bytes differed at {saved_path}")

    # GIO routes this request through OpenURI when sandboxed. Allow only the
    # expected no-handler errors after private payload preparation completes;
    # the test never assumes or controls a user's default opener.
    opened = harness.request("open_attachment", {"index": 0}, require_ok=False)
    harness.wait_for_attachment_completion(
        opened,
        "attachment OpenURI request",
        require_path=False,
        expected_action="prepare_open",
        allowed_errors=("No application is registered", "not supported"),
    )

    harness.request("open_compose")
    for command, value in (
        ("compose_set_from", "Distribution User <distribution@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", DRAFT_SUBJECT),
        ("compose_set_body", "This draft must survive a clean application restart."),
    ):
        harness.request(command, {"value": value})
    draft = harness.request("save_draft")
    report = draft.get("report")
    if not isinstance(report, dict):
        raise E2EFailure(f"draft save returned no report: {draft!r}")
    draft_path = Path(str(report.get("maildir_path")))
    draft_message_id = report.get("indexed_message_id")
    if not draft_path.is_file() or not isinstance(draft_message_id, str):
        raise E2EFailure(f"draft was not persisted and indexed: {draft!r}")
    return draft_message_id, draft_path


def exercise_portal_controls(
    app: AppProcess,
    work_root: Path,
    *,
    exercise_mailto_link: bool,
) -> None:
    """Drive the real native chooser/OpenURI seams under safe fixture policy."""

    harness = app.harness
    harness.wait_for_search()
    harness.select_subject("Attachment message")
    chooser = harness.request("save_selected_attachment", {"index": 0})
    if chooser.get("pending") is not True:
        raise E2EFailure(f"GtkFileChooserNative did not become pending: {chooser!r}")
    chooser_state = harness.request("attachment_test_state")
    if chooser_state.get("save_chooser", {}).get("visible") is not True:
        raise E2EFailure(f"attachment chooser was not visible: {chooser_state!r}")
    portal_target = work_root / "downloads" / "portal-selected.txt"
    completed = harness.request(
        "respond_attachment_save",
        {
            "id": chooser.get("chooser_id"),
            "response": "accept",
            "path": str(portal_target),
        },
    )
    completed = harness.wait_for_attachment_completion(
        completed,
        "chooser-selected attachment save",
        expected_action="save_to_target",
    )
    completed_path = Path(completed["path"])
    if b"attached text" not in completed_path.read_bytes():
        raise E2EFailure(f"chooser-selected attachment bytes differed: {completed!r}")
    opened = harness.request("open_attachment", {"index": 0})
    opened = harness.wait_for_attachment_completion(
        opened,
        "fixture attachment OpenURI seam",
        expected_action="prepare_open",
    )
    opened_state = harness.request("attachment_test_state")
    if opened_state.get("fake_opener") is not True:
        raise E2EFailure(
            f"fixture OpenURI seam did not use its safe opener: {opened_state!r}"
        )

    if exercise_mailto_link:
        harness.select_subject("HTML message")
        shown = harness.request("show_visual_html")
        if shown.get("html_view", {}).get("has_html") is not True:
            raise E2EFailure(f"fixture HTML message was not detected: {shown!r}")

        def html_visible() -> dict[str, Any] | None:
            state = harness.request("html_view_state")
            return state if state.get("html_visible") is True else None

        wait_until("fixture WebKitGTK HTML view", html_visible)
        harness.request("start_link_hints")

        def active_link_hints() -> dict[str, Any] | None:
            state = harness.request("link_hint_state")
            hints = state.get("link_hints", {})
            return state if hints.get("active") is True else None

        hint_state = wait_until("fixture HTML link hints", active_link_hints)
        labels = hint_state.get("link_hints", {}).get("labels")
        if not isinstance(labels, list) or len(labels) != 2:
            raise E2EFailure(f"fixture HTML links were unexpected: {hint_state!r}")
        mailto_label = labels[1]
        if not isinstance(mailto_label, str) or not mailto_label:
            raise E2EFailure(f"fixture mailto link had no label: {hint_state!r}")
        for key in mailto_label:
            harness.request("input_link_hint", {"key": key})

        def mailto_routed() -> dict[str, Any] | None:
            state = harness.request("app_state")
            fields = state.get("state", {}).get("compose_fields", {})
            return state if fields.get("to") == "fixture@example.test" else None

        wait_until("sandboxed HTML mailto link routing", mailto_routed)


def exercise_mailto_launch(app: AppProcess) -> None:
    expected = {
        "to": "routed@example.test",
        "subject": MAILTO_SUBJECT,
        "body": MAILTO_BODY,
    }

    def populated() -> dict[str, Any] | None:
        state = app.harness.request("app_state")
        fields = state.get("state", {}).get("compose_fields", {})
        return (
            state
            if all(fields.get(key) == value for key, value in expected.items())
            else None
        )

    wait_until("installed mailto launch route", populated)
    for command in (
        "compose_set_to",
        "compose_set_cc",
        "compose_set_bcc",
        "compose_set_subject",
        "compose_set_body",
    ):
        app.harness.request(command, {"value": ""})


def exercise_restart_and_send(
    app: AppProcess,
    smtp_state: SMTPState,
    expected_draft_message_id: str,
    expected_draft_path: Path,
) -> dict[str, Any]:
    harness = app.harness
    harness.wait_for_search()
    scheduled = harness.request("run_search", {"query": "tag:draft"})
    if scheduled.get("scheduled") is not True:
        raise E2EFailure(f"draft restart search was not scheduled: {scheduled!r}")
    state = harness.wait_for_search()
    rows = state.get("state", {}).get("thread_list_items")
    if not isinstance(rows, list) or not any(
        isinstance(row, dict) and row.get("subject") == DRAFT_SUBJECT for row in rows
    ):
        raise E2EFailure(f"saved draft was absent after restart: {state!r}")

    def reopened() -> dict[str, Any] | None:
        current = harness.request("app_state")
        active = current.get("state", {}).get("active_draft")
        if (
            isinstance(active, dict)
            and active.get("message_id") == expected_draft_message_id
            and active.get("path") == str(expected_draft_path)
        ):
            return current
        return None

    restart_state = wait_until("persisted draft reopening", reopened)
    compose = restart_state.get("state", {}).get("compose_fields", {})
    if compose.get("subject") != DRAFT_SUBJECT:
        raise E2EFailure(f"reopened draft lost its fields: {restart_state!r}")

    harness.request("compose_set_subject", {"value": SMTP_SUBJECT})
    harness.request(
        "compose_set_body", {"value": "Captured by a loopback-only SMTP server."}
    )
    started = harness.request("compose_send")
    if (
        started.get("pending_confirmation") is True
        or started.get("pending") is not True
    ):
        pending = harness.request("pending_confirmation")
        confirmation = pending.get("pending")
        if not isinstance(confirmation, dict):
            raise E2EFailure(
                f"send neither started nor requested confirmation: {started!r}"
            )
        accepted = harness.request(
            "respond_confirmation", {"id": confirmation.get("id"), "response": "accept"}
        )
        if accepted.get("ok") is not True:
            raise E2EFailure(f"send confirmation failed: {accepted!r}")
    send_state = harness.wait_for_send()
    if not smtp_state.event.wait(timeout=10):
        raise E2EFailure(
            "SMTP helper reported success without a captured SMTP DATA transaction"
        )
    captured = smtp_state.capture_path.read_bytes()
    message = BytesParser(policy=email.policy.default).parsebytes(captured)
    if message.get("Subject") != SMTP_SUBJECT:
        raise E2EFailure(
            f"SMTP capture had the wrong Subject: {message.get('Subject')!r}"
        )
    body = message.get_body(preferencelist=("plain",))
    if body is None or "loopback-only SMTP server" not in body.get_content():
        raise E2EFailure("SMTP capture lost the composed body")
    if not smtp_state.recipients:
        raise E2EFailure("SMTP capture observed no RCPT command")
    return send_state


def main() -> int:
    args = parse_args()
    if args.preserve_home and args.work_root is None:
        raise E2EFailure("--preserve-home requires --work-root")
    if args.require_display and not (
        os.environ.get("WAYLAND_DISPLAY") or os.environ.get("DISPLAY")
    ):
        raise E2EFailure("a required offscreen Wayland/X11 display is not available")
    for executable in (args.notm, args.notmuch):
        if not executable.is_file() or not os.access(executable, os.X_OK):
            raise E2EFailure(f"required executable is not executable: {executable}")

    created_temp = args.work_root is None
    if args.work_root is None:
        work_root = Path(tempfile.mkdtemp(prefix="notm-distribution-e2e."))
    else:
        work_root = args.work_root.resolve()
        work_root.mkdir(parents=True, exist_ok=True)
        if any(work_root.iterdir()):
            raise E2EFailure(f"--work-root must be empty: {work_root}")
    environment = clean_environment(work_root, preserve_home=args.preserve_home)
    capture_path = work_root / "smtp-capture.eml"
    smtp_state = SMTPState(capture_path)
    smtp_server = SMTPServer(smtp_state)
    smtp_thread = threading.Thread(target=smtp_server.serve_forever, daemon=True)
    smtp_thread.start()
    smtp_port = int(smtp_server.server_address[1])

    first: AppProcess | None = None
    second: AppProcess | None = None
    portal_fixture: AppProcess | None = None
    mailto_app: AppProcess | None = None
    try:
        app_config, notmuch_config, _drafts, sent = configure_mail(
            work_root,
            environment,
            args.notmuch,
            args.smtp_command,
            smtp_port,
        )
        version = run_checked([str(args.notm), "--version"], environment).stdout.strip()
        if not version.startswith("notm "):
            raise E2EFailure(
                f"installed executable returned an invalid version: {version!r}"
            )

        first = AppProcess(args.notm, app_config, work_root, environment, 1)
        draft_message_id, draft_path = exercise_first_run(first, work_root)
        first.close()
        first = None

        second = AppProcess(args.notm, app_config, work_root, environment, 2)
        send_state = exercise_restart_and_send(
            second, smtp_state, draft_message_id, draft_path
        )
        second.close()
        second = None

        portal_fixture = AppProcess(
            args.notm,
            None,
            work_root,
            environment,
            3,
            fixture=True,
        )
        exercise_portal_controls(
            portal_fixture,
            work_root,
            exercise_mailto_link=args.exercise_portal_link,
        )
        portal_fixture.close()
        portal_fixture = None

        mailto_target = (
            "mailto:routed@example.test?subject=Distribution%20mailto%20routing"
            "&body=Opened%20through%20the%20installed%20mailto%20launch%20route."
        )
        mailto_app = AppProcess(
            args.notm,
            app_config,
            work_root,
            environment,
            4,
            launch_target=mailto_target,
        )
        exercise_mailto_launch(mailto_app)
        mailto_app.close()
        mailto_app = None

        index_environment = environment.copy()
        index_environment["NOTMUCH_CONFIG"] = str(notmuch_config)
        sent_count = run_checked(
            [str(args.notmuch), "count", f'subject:"{SMTP_SUBJECT}" and tag:sent'],
            index_environment,
        ).stdout.strip()
        if sent_count != "1":
            raise E2EFailure(f"expected one indexed sent copy, got {sent_count!r}")
        if not any((sent / directory).iterdir() for directory in ("new", "cur")):
            raise E2EFailure("the configured Sent Maildir contains no message")

        evidence = {
            "ok": True,
            "version": version,
            "plain_text": True,
            "html_webkitgtk": True,
            "attachment_save": True,
            "attachment_native_chooser": True,
            "mailto_launch": True,
            "portal_mailto_link": args.exercise_portal_link,
            "draft_restart": True,
            "smtp_capture": True,
            "smtp_recipients": smtp_state.recipients,
            "sent_copy": send_state.get("state", {}).get("last_send_report", {}),
            "work_root": str(work_root),
        }
        print(json.dumps(evidence, indent=2, sort_keys=True))
        return 0
    except BaseException:
        print(f"distribution E2E evidence retained at {work_root}", file=sys.stderr)
        for log in sorted(work_root.glob("notm-run-*.log")):
            print(f"--- {log.name} ---", file=sys.stderr)
            print(log.read_text(encoding="utf-8", errors="replace"), file=sys.stderr)
        raise
    finally:
        if first is not None:
            first.abort()
        if second is not None:
            second.abort()
        if portal_fixture is not None:
            portal_fixture.abort()
        if mailto_app is not None:
            mailto_app.abort()
        smtp_server.shutdown()
        smtp_server.server_close()
        smtp_thread.join(timeout=5)
        if created_temp and not args.keep_work and sys.exc_info()[0] is None:
            shutil.rmtree(work_root)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except E2EFailure as error:
        print(f"distribution_e2e: {error}", file=sys.stderr)
        raise SystemExit(1)
