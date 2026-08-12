#!/usr/bin/env python3
"""Black-box PTY regression for disabling the post-query Tab reminder."""

from __future__ import annotations

import errno
import json
import os
from pathlib import Path
import pty
import select
import socket
import subprocess
import sys
import tempfile
import threading
import time


COMMAND = "pwd"
REMINDER = "Press Tab at an empty prompt to edit this command."
SESSION = "zsh-tab-hint-test"


def fail(message: str, terminal_output: str = "") -> None:
    if terminal_output:
        message = f"{message}\n--- terminal output ---\n{terminal_output}"
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def serve_once(listener: socket.socket, errors: list[BaseException]) -> None:
    try:
        connection, _ = listener.accept()
        with connection:
            connection.settimeout(10)
            request = bytearray()
            expected_length: int | None = None
            while expected_length is None or len(request) < expected_length:
                chunk = connection.recv(4096)
                if not chunk:
                    raise RuntimeError("provider client closed before sending a full request")
                request.extend(chunk)
                if expected_length is None and b"\r\n\r\n" in request:
                    headers, body = bytes(request).split(b"\r\n\r\n", 1)
                    content_length = 0
                    for line in headers.split(b"\r\n")[1:]:
                        name, separator, value = line.partition(b":")
                        if separator and name.lower() == b"content-length":
                            content_length = int(value.strip())
                            break
                    expected_length = len(headers) + 4 + content_length
                    if not headers.startswith(b"POST /v1/chat/completions "):
                        raise RuntimeError("provider received an unexpected request route")
                    if len(body) > content_length:
                        raise RuntimeError("provider received an oversized request body")

            response_body = json.dumps(
                {
                    "choices": [
                        {
                            "message": {"role": "assistant", "content": COMMAND},
                            "finish_reason": "stop",
                        }
                    ]
                },
                separators=(",", ":"),
            ).encode()
            connection.sendall(
                b"HTTP/1.1 200 OK\r\n"
                b"Content-Type: application/json\r\n"
                + f"Content-Length: {len(response_body)}\r\n".encode()
                + b"Connection: close\r\n\r\n"
                + response_body
            )
    except BaseException as error:  # Report server-thread failures in the main thread.
        errors.append(error)
    finally:
        listener.close()


def run_in_pty(arguments: list[str], environment: dict[str, str]) -> tuple[int, str]:
    master, slave = pty.openpty()
    process = subprocess.Popen(
        arguments,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=environment,
        close_fds=True,
    )
    os.close(slave)
    output = bytearray()
    deadline = time.monotonic() + 15
    try:
        while True:
            if time.monotonic() >= deadline:
                process.kill()
                process.wait()
                fail("HowTo query timed out", output.decode(errors="replace"))
            readable, _, _ = select.select([master], [], [], 0.1)
            if readable:
                try:
                    chunk = os.read(master, 4096)
                except OSError as error:
                    if error.errno == errno.EIO and process.poll() is not None:
                        break
                    raise
                if not chunk:
                    break
                output.extend(chunk)
                continue
            if process.poll() is not None:
                break
    finally:
        os.close(master)
    return process.wait(), output.decode(errors="replace").replace("\r\n", "\n")


def main() -> None:
    project_root = Path(__file__).resolve().parent.parent
    binary = Path(
        os.environ.get("HOWTO_TEST_BINARY", project_root / "target" / "debug" / "howto")
    ).resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        fail(f"HowTo test binary is missing or not executable: {binary}")

    with tempfile.TemporaryDirectory(prefix="howto-tab-hint-") as temporary:
        root = Path(temporary)
        home = root / "home"
        state = root / "state"
        home.mkdir(mode=0o700)
        state.mkdir(mode=0o700)
        provider_socket = root / "provider.sock"

        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        listener.bind(str(provider_socket))
        listener.listen(1)
        listener.settimeout(10)
        server_errors: list[BaseException] = []
        server = threading.Thread(
            target=serve_once,
            args=(listener, server_errors),
            name="fake-howto-provider",
        )
        server.start()

        (state / "config.json").write_text(
            json.dumps(
                {
                    "schema": 1,
                    "server_url": f"unix://{provider_socket}",
                    "show_tab_hint": False,
                }
            )
            + "\n"
        )
        setup_file = state / "setup.json"
        setup_file.write_text(
            json.dumps(
                {
                    "schema": 1,
                    "completed_at": int(time.time()),
                    "shell": "zsh",
                    "shell_integration_schema": 1,
                    "shell_startup_files": [],
                }
            )
            + "\n"
        )
        setup_file.chmod(0o600)

        environment = os.environ.copy()
        environment.update(
            {
                "HOME": str(home),
                "HOWTO_HOME": str(state),
                "HOWTO_SHELL_SESSION": SESSION,
                "NO_COLOR": "1",
            }
        )
        for name in ("HOWTO_API_KEY", "HOWTO_LLAMA_SERVER", "HOWTO_MODEL"):
            environment.pop(name, None)

        status, terminal_output = run_in_pty(
            [str(binary), "show", "current", "directory"], environment
        )
        server.join(timeout=10)
        if server.is_alive():
            fail("fake provider did not finish", terminal_output)
        if server_errors:
            fail(f"fake provider failed: {server_errors[0]}", terminal_output)
        if status != 0:
            fail(f"HowTo query exited with status {status}", terminal_output)
        if COMMAND not in terminal_output.splitlines():
            fail("HowTo did not display the generated command", terminal_output)
        if REMINDER in terminal_output:
            fail("show_tab_hint=false did not suppress the Tab reminder", terminal_output)

        taken = subprocess.run(
            [str(binary), "shell", "take"],
            env=environment,
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        if taken.returncode != 0 or taken.stdout != f"{COMMAND}\n":
            fail(
                "the reminder setting also suppressed the staged command; "
                f"shell take exited {taken.returncode} with stdout {taken.stdout!r} "
                f"and stderr {taken.stderr!r}",
                terminal_output,
            )

        consumed = subprocess.run(
            [str(binary), "shell", "take"],
            env=environment,
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        if consumed.returncode != 1 or consumed.stdout:
            fail("the staged command was not consumed exactly once", terminal_output)

    print("PASS: show_tab_hint=false hides only the PTY reminder")


if __name__ == "__main__":
    main()
