#!/usr/bin/env python3
"""Black-box PTY test for the managed failed-command advisor path."""

from __future__ import annotations

import errno
import json
import os
from pathlib import Path
import pty
import select
import shlex
import signal
import subprocess
import sys
import tempfile
import time


FAILED_COMMAND = "lls -h"
SUGGESTION = "ls -h"
SESSION = "zsh-advisor-binary-test"
UNAVAILABLE_EXIT = 78


def fail(message: str, terminal_output: str = "") -> None:
    if terminal_output:
        message = f"{message}\n--- terminal output ---\n{terminal_output}"
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2) + "\n")
    path.chmod(0o600)


def run_captured(
    arguments: list[str], environment: dict[str, str], *, cwd: Path
) -> subprocess.CompletedProcess[bytes]:
    try:
        return subprocess.run(
            arguments,
            cwd=cwd,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=10,
        )
    except subprocess.TimeoutExpired as error:
        fail(f"command timed out: {' '.join(arguments)}\n{error}")


def run_advisor_in_pty(
    binary: Path,
    environment: dict[str, str],
    cwd: Path,
    command: str,
    terminal_history: tuple[str, ...] = (),
) -> tuple[int, str]:
    """Pipe command text to stdin while keeping both outputs on one PTY."""

    master, slave = pty.openpty()
    for line in terminal_history:
        os.write(slave, f"{line}\n".encode())
    process = subprocess.Popen(
        [str(binary), "shell", "advise", "--status", "127"],
        cwd=cwd,
        env=environment,
        stdin=subprocess.PIPE,
        stdout=slave,
        stderr=slave,
        close_fds=True,
    )
    os.close(slave)
    output = bytearray()
    deadline = time.monotonic() + 25
    try:
        assert process.stdin is not None
        process.stdin.write(command.encode())
        process.stdin.close()

        while True:
            if time.monotonic() >= deadline:
                process.kill()
                process.wait()
                fail("failed-command advisor timed out", output.decode(errors="replace"))

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
        if process.poll() is None:
            process.kill()
            process.wait()
        os.close(master)

    terminal_output = output.decode(errors="replace").replace("\r\n", "\n")
    return process.wait(), terminal_output


def make_fake_server(
    path: Path,
    started_log: Path,
    request_log: Path,
) -> None:
    python = str(Path(sys.executable).resolve())
    source = f'''#!{python}
import json
import os
from pathlib import Path
import socketserver
import sys
from http.server import BaseHTTPRequestHandler

STARTED_LOG = Path({str(started_log)!r})
REQUEST_LOG = Path({str(request_log)!r})


def append_record(record):
    with REQUEST_LOG.open("a") as output:
        output.write(json.dumps(record, separators=(",", ":")) + "\\n")
        output.flush()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, _format, *_arguments):
        pass

    def send_json(self, status, value):
        body = json.dumps(value, separators=(",", ":")).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)
        self.close_connection = True

    def do_GET(self):
        append_record({{"method": "GET", "path": self.path}})
        if self.path == "/health":
            self.send_json(200, {{"status": "ok"}})
        else:
            self.send_json(404, {{"error": "unexpected route"}})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        append_record({{
            "method": "POST",
            "path": self.path,
            "authorization": self.headers.get("Authorization"),
            "body": json.loads(body),
        }})
        if self.path != "/v1/chat/completions":
            self.send_json(404, {{"error": "unexpected route"}})
            return
        self.send_json(200, {{
            "choices": [{{
                "message": {{"role": "assistant", "content": {SUGGESTION!r}}},
                "finish_reason": "stop",
            }}]
        }})


host_index = sys.argv.index("--host") + 1
socket_path = sys.argv[host_index]
STARTED_LOG.write_text(json.dumps({{
    "argv": sys.argv,
    "environment": dict(os.environ),
}}, separators=(",", ":")) + "\\n")
with socketserver.UnixStreamServer(socket_path, Handler) as server:
    server.serve_forever()
'''
    path.write_text(source)
    path.chmod(0o700)


def stop_managed_server(binary: Path, environment: dict[str, str], state: Path) -> None:
    state_file = state / "run" / "server.json"
    pid: int | None = None
    if state_file.is_file():
        try:
            pid = int(json.loads(state_file.read_text())["pid"])
        except (KeyError, TypeError, ValueError, json.JSONDecodeError):
            pass

    try:
        subprocess.run(
            [str(binary), "server", "stop"],
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
            timeout=10,
        )
    except subprocess.TimeoutExpired:
        pass

    if pid is None:
        return
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    for _ in range(20):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        time.sleep(0.05)
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def assert_no_server_contact(started_log: Path, request_log: Path, context: str) -> None:
    if started_log.exists() or request_log.exists():
        fail(f"{context} contacted the managed model server")


def main() -> None:
    project_root = Path(__file__).resolve().parent.parent
    binary = Path(
        os.environ.get("HOWTO_TEST_BINARY", project_root / "target" / "debug" / "howto")
    ).resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        fail(f"HowTo test binary is missing or not executable: {binary}")

    # Keep the managed Unix socket comfortably below macOS's 103-byte limit.
    with tempfile.TemporaryDirectory(
        prefix="howto-advisor-bin-", dir="/tmp"
    ) as temporary:
        root = Path(temporary)
        home = root / "home"
        state = root / "state"
        fake_bin = root / "bin"
        cwd_sentinel = "HOWTO_CWD_SENTINEL_19f48d"
        command_cwd = root / cwd_sentinel
        for directory in (home, state, fake_bin, command_cwd):
            directory.mkdir(mode=0o700)

        started_log = root / "server-started.json"
        request_log = root / "server-requests.jsonl"
        execution_marker = root / "suggestion-was-executed"
        fake_server = fake_bin / "llama-server"
        make_fake_server(fake_server, started_log, request_log)

        model = root / "fixture.gguf"
        with model.open("wb") as output:
            output.write(b"GGUF")
            output.seek(1_048_576)
            output.write(b"\0")
        model.chmod(0o600)

        fake_ls = fake_bin / "ls"
        fake_ls.write_text(
            "#!/bin/sh\n"
            f"printf executed > {shlex.quote(str(execution_marker))}\n"
        )
        fake_ls.chmod(0o700)

        config = {
            "schema": 1,
            "model_path": str(model),
            "llama_server_path": str(fake_server),
            "startup_timeout_seconds": 5,
            "failed_command_advisor": False,
        }
        write_json(state / "config.json", config)
        write_json(
            state / "setup.json",
            {
                "schema": 1,
                "completed_at": int(time.time()),
                "shell": "zsh",
                "shell_integration_schema": 2,
                "shell_startup_files": [],
                "failed_command_advisor_prompted": True,
            },
        )

        sentinels = {
            "environment": "HOWTO_ENV_SENTINEL_dfa3a4",
            "stdout": "HOWTO_STDOUT_SENTINEL_7c4b53",
            "stderr": "HOWTO_STDERR_SENTINEL_9aef21",
            "cwd": cwd_sentinel,
        }
        environment = os.environ.copy()
        environment.update(
            {
                "HOME": str(home),
                "HOWTO_HOME": str(state),
                "HOWTO_SHELL_SESSION": SESSION,
                "HOWTO_TEST_ENV_VALUE": sentinels["environment"],
                "HOWTO_TEST_PREVIOUS_STDOUT": sentinels["stdout"],
                "HOWTO_TEST_PREVIOUS_STDERR": sentinels["stderr"],
                "NO_COLOR": "1",
                "PATH": f"{fake_bin}{os.pathsep}{environment.get('PATH', '')}",
            }
        )
        for key in ("HOWTO_MODEL", "HOWTO_LLAMA_SERVER", "HOWTO_API_KEY"):
            environment.pop(key, None)

        # Make accidental model/runtime resolution fail loudly in the disabled
        # phase. The readiness gate must need neither file nor start a process.
        model.chmod(0)
        fake_server.chmod(0)
        ready = run_captured(
            [str(binary), "shell", "advisor-ready"], environment, cwd=command_cwd
        )
        if ready.returncode != UNAVAILABLE_EXIT or ready.stdout or ready.stderr:
            fail(
                "disabled advisor readiness was not a silent exit 78",
                (ready.stdout + ready.stderr).decode(errors="replace"),
            )
        assert_no_server_contact(started_log, request_log, "disabled readiness")

        disabled_code, disabled_output = run_advisor_in_pty(
            binary, environment, command_cwd, FAILED_COMMAND
        )
        if disabled_code != UNAVAILABLE_EXIT or disabled_output:
            fail(
                "disabled advisor invocation was not a silent exit 78",
                disabled_output,
            )
        assert_no_server_contact(started_log, request_log, "disabled advice")

        model.chmod(0o600)
        fake_server.chmod(0o700)
        config["failed_command_advisor"] = True
        write_json(state / "config.json", config)

        try:
            exit_code, terminal_output = run_advisor_in_pty(
                binary,
                environment,
                command_cwd,
                FAILED_COMMAND,
                (sentinels["stdout"], sentinels["stderr"]),
            )
            if exit_code != 0:
                fail(f"enabled advisor exited with status {exit_code}", terminal_output)
            for channel in ("stdout", "stderr"):
                if sentinels[channel] not in terminal_output:
                    fail(
                        f"test fixture did not place its {channel} sentinel on the PTY",
                        terminal_output,
                    )
            expected_output = (
                "HowTo beta suggestion (review only; not executed):\n"
                f"  {SUGGESTION}\n"
            )
            if expected_output not in terminal_output:
                fail("enabled advisor did not print its review-only suggestion", terminal_output)

            if not started_log.is_file():
                fail("enabled advisor did not start the configured managed server")
            if not request_log.is_file():
                fail("enabled advisor made no model requests")
            records = [
                json.loads(line)
                for line in request_log.read_text().splitlines()
                if line.strip()
            ]
            completions = [
                record
                for record in records
                if record.get("method") == "POST"
                and record.get("path") == "/v1/chat/completions"
            ]
            if len(completions) != 1:
                fail(f"advisor made {len(completions)} completion requests; expected one")
            if any(
                record.get("path") not in {"/health", "/v1/chat/completions"}
                for record in records
            ):
                fail(f"managed server received an unexpected request: {records!r}")

            completion = completions[0]
            if not str(completion.get("authorization", "")).startswith("Bearer "):
                fail("managed completion request omitted its local bearer token")
            body = completion.get("body")
            try:
                user_payload = json.loads(body["messages"][1]["content"])
            except (KeyError, IndexError, TypeError, json.JSONDecodeError) as error:
                fail(f"advisor completion request had an invalid user payload: {error}")
            if user_payload != {
                "failed_command": FAILED_COMMAND,
                "exit_status": 127,
            }:
                fail(f"advisor sent unexpected failed-command data: {user_payload!r}")

            serialized_request = json.dumps(body, sort_keys=True)
            leaked = [value for value in sentinels.values() if value in serialized_request]
            if leaked:
                fail(f"advisor leaked non-command context to the model: {leaked!r}")
            server_start = json.loads(started_log.read_text())
            if sentinels["environment"] in json.dumps(
                server_start.get("environment", {}), sort_keys=True
            ):
                fail("managed server inherited the caller's unrelated environment")

            if execution_marker.exists():
                fail("advisor executed the suggested command")
            pending = list((state / "run").glob("pending-*.json"))
            if pending:
                fail(f"advisor staged its suggestion for Tab: {pending!r}")
            take = run_captured(
                [str(binary), "shell", "take"], environment, cwd=command_cwd
            )
            if take.returncode != 1 or take.stdout or take.stderr:
                fail(
                    "advisor left a command available to the Tab handoff",
                    (take.stdout + take.stderr).decode(errors="replace"),
                )
        finally:
            stop_managed_server(binary, environment, state)

    print("PASS: managed failed-command advisor binary boundary")


if __name__ == "__main__":
    main()
