# Troubleshooting

Start with the built-in diagnostics:

```sh
howto doctor --deep
howto model status --deep
howto server status
howto shell status
howto config list
```

`--deep` reads and hashes a matching managed or packaged copy of the full
default model, so it can take a little while. Custom models have no built-in
expected digest. Commands return nonzero when the model is missing, default
model deep verification fails, or `doctor` finds setup incomplete.

## Exit codes

| Code | Class |
|---:|---|
| `0` | Success, including a declined execution confirmation. |
| `1` | Execution, filesystem, or JSON failure; also a negative status such as a missing model. |
| `2` | Invalid CLI use or request. |
| `3` | Configuration, dependency, or model problem. |
| `4` | Server, network, provider-response, or generated-output problem. |
| other | Exit status of a command that you explicitly approved with `--execute`. |

## Homebrew cannot find HowTo

Install the stable tap formula directly:

```sh
brew install jiwidi/tap/howto
```

Confirm that both names resolve:

```sh
brew list howto
howto --version
```

If the installed formula is stale, update the tap and reinstall:

```sh
brew update
brew reinstall jiwidi/tap/howto
```

## Setup has not been completed

Run the one-time onboarding workflow before a normal query:

```sh
howto setup
```

An interactive query offers to run it. Piped input, JSON, quiet mode, and other
non-interactive requests never prompt or start a large download; they exit with
an instruction to run setup. `howto setup --yes` accepts setup prompts for
automation, while `--no-shell` disables and removes any HowTo-managed Tab
integration. Setup uses an existing verified model or configured provider and
is safe to rerun after an interruption. If `setup.json` was truncated, explicit
setup quarantines it as `setup.json.corrupt-*` and repairs onboarding; unsafe
files and state from a newer incompatible HowTo version remain hard errors.

## The model is not installed

`howto setup` asks before downloading about 940 MiB. For unattended setup, use:

```sh
howto setup --yes
howto model status --deep
```

The installer needs the remaining model bytes plus 128 MiB of working space.
An interrupted download stays as a `.part` file and is resumed on the next
install attempt. Concurrent installers wait on a private lock instead of
downloading twice. Resume responses must contain an exact `Content-Range`; an
invalid range or HTTP 416 removes the partial, and the next install starts
again. A response that would exceed the pinned artifact size is rejected
before the oversized chunk is written.

If a resume repeatedly fails, inspect the model directory listed in
[Privacy](../PRIVACY.md). Move the
`nl2sh-1.5b-Q4_K_M.gguf.part` file aside, then retry. Do not replace the
managed filename with an unverified download.

## Model verification failed

Run:

```sh
howto model status --deep
howto server stop
```

The initial managed download is verified before installation, but normal
startup uses the pinned artifact's path and exact byte size for a fast check.
`--deep` is the explicit full SHA-256 verification.

If the deep check says `FAILED`, run `howto model install --yes`. Explicit managed
installation hashes an existing artifact and replaces it when verification
fails. You do not need to remove it manually.

A custom `model_path` or `HOWTO_MODEL` is accepted when it is a regular GGUF
larger than 1 MiB with a `GGUF` header. Stored config paths must be absolute;
environment override paths are canonicalized. HowTo does not know that
model's expected digest.
`--deep` verifies only an artifact matching the built-in default manifest.
Verify custom models against their publisher's signed metadata.

## `llama-server` was not found

Homebrew installation should supply it as the `llama.cpp` dependency. Repair
or locate it with:

```sh
brew install llama.cpp
command -v llama-server
```

If it exists outside `PATH`, configure the full executable path:

```sh
howto config set llama_server_path "$(brew --prefix llama.cpp)/bin/llama-server"
howto doctor
```

Resolution order is `HOWTO_LLAMA_SERVER`, `llama_server_path`, `PATH`, then
well-known Homebrew and system paths. The selected file must exist and be
executable, and its resolved filename must be `llama-server`. `howto doctor` also
runs `llama-server --version` with a 30-second deadline; a failed probe keeps
local readiness false. Run a harmless query to test model loading and inference.

## Tab does not insert the generated command

Check both persistent configuration and the current shell:

```sh
howto shell status
```

If configuration is present but the current shell is not active, open a new
terminal. Tab inserts only at a completely empty prompt, only for a fresh
single-command `NO_KNOWN_RISK` or `CAUTION` result, and consumes that result
once. It otherwise retains normal completion behavior. Pending commands expire
after ten minutes and are isolated per shell session.

Repair or select the integration explicitly with:

```sh
howto shell enable --shell zsh   # or bash/fish
```

Bash needs version 4.1 or newer for its empty-line completion API. The system
Bash 3.2 shipped by macOS is intentionally left unchanged; use the default zsh
or a newer Bash. If setup refused a symlinked or malformed startup file, resolve
that configuration manually and rerun setup rather than replacing the file.
`howto shell disable` removes the selected or receipt-tracked integration;
`howto setup --no-shell` sweeps every HowTo-managed shell integration.

## The local server does not start

First stop any recorded managed process and retry:

```sh
howto server stop
howto "show my current directory"
```

HowTo includes the last several runtime-log lines in startup errors. The full
log is normally:

- macOS: `~/Library/Logs/HowTo/llama-server.log`
- Linux: `${XDG_STATE_HOME:-~/.local/state}/howto/log/llama-server.log`
- `HOWTO_HOME`: `$HOWTO_HOME/logs/llama-server.log`

If `server.json` is malformed, HowTo quarantines it beside the original as a
timestamped `server.json.corrupt-*` file, cleans the related live-state files,
and prints a warning. Preserve that file only if it is useful for a sanitized
bug report.

Common causes are an incompatible `llama-server`, an invalid custom model,
insufficient memory, or a startup deadline that is too short. For a slow
machine, increase the deadline within its 1–600 second range:

```sh
howto config set startup_timeout_seconds 180
```

The managed runtime uses the `llama-server` CLI installed by Homebrew. A future
llama.cpp release can change flags or GGUF compatibility independently of
HowTo. Include both `howto --version` and `llama-server --version` in a
sanitized bug report.

## The Unix socket path is too long

Unix sockets have a small operating-system path limit. This most often happens
when `HOWTO_HOME` points into a deeply nested directory. Unset it to use the
normal runtime location, or choose a short, absolute, private path. Values for
`HOWTO_HOME`, Linux XDG directory overrides, and fallback `TMPDIR` are rejected
when relative. Do not point the runtime at a shared directory: it contains
local authentication and process state.

## Server status says stopped

The model server starts lazily on a query. A stopped result before the first
query is normal:

```sh
howto "show my current directory"
howto server status
```

The managed runtime is configured to sleep after 15 minutes idle. Use
`howto server stop` when you need to release it immediately. `server status`
describes only the managed local process; it does not probe a configured
provider. Status checks serialize with lifecycle changes; normal query reuse
allows a waking managed server up to three health attempts before replacing
it.

## A configured provider fails

HowTo expects a llama.cpp-compatible OpenAI-style chat endpoint. Configure
the base URL without `/v1/chat/completions`, because HowTo appends that route:

```sh
howto config set server_url https://provider.example
howto config set model_id provider-model-name
export HOWTO_API_KEY='...'
```

The server must accept the chat messages plus `max_tokens`, `temperature`,
`top_p`, `n`, `repeat_penalty`, `repeat_last_n`, `stop`, and `stream`.
HowTo always sends `n: 1`; additional alternatives are separate requests.
Providers that implement only a subset of the OpenAI API may reject the
llama.cpp-specific repeat settings.

For a same-host Unix socket:

```sh
howto config set server_url unix:///absolute/path/to/provider.sock
```

The path must be absolute. HTTPS and HTTP are also accepted; use HTTPS for any
remote host. An HTTP(S) base URL may contain a path, but not credentials, a
query, or a fragment. A key is read only from `HOWTO_API_KEY`.

`howto doctor`, including `--deep`, tries `GET /health` and then, if needed,
`GET /v1/models`, with a five-second deadline for each request and
`HOWTO_API_KEY` as a bearer token when set. It reports configured mode ready
only when one of those routes returns a successful status. This does not test
`POST /v1/chat/completions`; run a harmless query for an end-to-end
compatibility check. Inspect or reset provider state:

```sh
howto config get server_url
howto config get model_id
howto config unset server_url
howto config unset model_id
```

## The model response was rejected

HowTo retries once with a stricter instruction when the first result cannot
be reduced to one valid command. It then fails if the provider returns:

- no choice or no command;
- invalid JSON or an API error;
- terminal control bytes;
- multiple physical lines or malformed Markdown;
- more than 4096 command bytes;
- a response that ended at the token limit.

Malformed or incomplete shell syntax that still passes the one-line shape
checks is assessed as `UNKNOWN`, not retried. It can be displayed or copied for
inspection, but quiet output and execution are blocked. Provider API error
text shown by HowTo is capped at 2048 bytes and has terminal control and
bidirectional-format characters escaped.

Try a more specific request. With a custom provider, confirm that it returns
`choices[].message.content` and a normal `finish_reason`. Increasing
`max_tokens` may help a genuinely long command, but HowTo always enforces the
one-line and 4096-byte limits. It must remain within 16–4096 and be strictly
smaller than `context_size`.

## `--quiet` refuses the result

Quiet output is deliberately limited to `NO_KNOWN_RISK`:

```text
quiet output is blocked for a CAUTION command; review it without --quiet
```

Run the same request without `--quiet` to see the rule findings. Do not work
around the gate with output filtering or automatic evaluation. A
`NO_KNOWN_RISK` result is still untrusted and should not be fed to `eval`.
Quiet mode cannot be combined with `--json`, `--execute`, or `--count` greater
than 1.

## `--execute` will not run

Execution requires a real terminal for confirmation. It is intentionally
unavailable through a non-interactive pipe or job. Queries, diagnostics, and
model, server, or config operations also refuse root, effective credentials
different from the real user, and `sudo`.

Risk gates are fixed:

- `NO_KNOWN_RISK`: type exactly `y`;
- `CAUTION`: type exactly `RUN`;
- `DANGER`: never executed; and
- `UNKNOWN`: never executed.

If you requested multiple alternatives with `--count` and also used
`--execute` or `--copy`, selection is interactive as well.

The default zsh and bash commands run without startup files. Configured sh and
dash are also supported. `HOWTO_API_KEY`, startup hooks, exported Bash
functions, dynamic-loader variables, and common language-runtime injection
variables are removed first. If a command depends on an alias, function, or
profile-only `PATH`, ask for an explicit executable or adjust the request. The
executed command's exit status becomes HowTo's exit status.

`--json` and `--execute` cannot be combined: execution output would corrupt the
machine-readable stream. Generate JSON for inspection, then make any execution
decision in a separate interactive invocation.

## Clipboard copying fails

macOS uses `/usr/bin/pbcopy`, which is part of the operating system. Linux
tries `wl-copy`, then `xclip`, then `xsel`. Install one suitable for your
desktop session. If an available helper fails, HowTo tries the remaining
helpers and reports each failed attempt; check the desktop display and
clipboard session named by the final error.

Copying occurs before the execution gate and supports every risk class. A
successful copy does not mean the command was approved as safe.

## The generated command uses the wrong platform syntax

HowTo tells the model whether it is targeting macOS/zsh/BSD tools or
Linux/bash/GNU tools, and the safety checker warns about several incompatible
commands and flags. This cannot catch every portability error.

State the intended tool or platform constraint explicitly, request alternatives
with `-n`, and inspect flags in the system manual before execution:

```sh
howto -n 3 "on macOS, show files larger than 1 GB using built-in tools"
```

## Configuration errors

Use the CLI instead of hand-editing JSON:

```sh
howto config path
howto config list --json
howto config unset KEY
```

Numeric settings are range-checked, including `max_tokens < context_size`.
Provider URL shapes, model IDs, and execution-shell names are validated.
Config schema 0, future schemas, and unknown keys are refused. Stored model,
runtime, and shell paths and `HOWTO_HOME` must be absolute. HowTo will not
read a symlinked config file or use a data directory owned by another user.

## Preparing a bug report

Include:

```sh
howto --version
howto doctor --json
howto model status --json
howto server status --json
```

Add `--deep` only when model integrity is relevant. Redact usernames, private
paths, endpoint URLs, prompts, generated commands, and runtime logs before
posting. Never include `HOWTO_API_KEY`, the managed `server.key`, or another
secret. Report vulnerabilities privately as described in
[SECURITY.md](../SECURITY.md).
