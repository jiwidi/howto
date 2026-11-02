# Troubleshooting

Start with the built-in diagnostics:

```sh
howto doctor --deep
howto model status --deep
howto server status
howto shell status
howto config list
```

`--deep` reads and hashes a matching managed or packaged catalog bundle, so it
can take a little while. Custom models have no built-in expected digest.
Commands return nonzero when the selected model is missing, deep verification
fails, or `doctor` finds setup incomplete.

## Exit codes

| Code | Class |
|---:|---|
| `0` | Success, including a declined execution confirmation. |
| `1` | Execution, filesystem, or JSON failure; also a negative status such as a missing model. |
| `2` | Invalid CLI use or request. |
| `3` | Configuration, dependency, or model problem. |
| `4` | Embedded-engine, network, provider-response, or generated-output problem. |
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
an instruction to run setup. `howto setup --yes` accepts default-Yes prompts
for automation, while `--no-shell` disables and removes any HowTo-managed shell
integration. `--yes` never opts in to the beta failed-command advisor or
approves a startup symlink. Setup uses an existing verified model or configured
provider and is safe to rerun after an interruption. Existing schema-1
installations keep the `legacy` profile. When new profiles are qualified, use
`--model compact|balanced|accurate` to make an explicit selection. If
`setup.json` was
truncated, explicit setup quarantines it as `setup.json.corrupt-*` and repairs
onboarding; unsafe files and state from a newer incompatible HowTo version
remain hard errors.

## The model is not installed

`howto setup` asks before downloading the selected profile. The only currently
published catalog entry is the approximately 940 MiB legacy model. For
unattended setup, use:

```sh
howto setup --yes
howto model status --deep
```

The installer needs the remaining model bytes plus 128 MiB of working space.
An interrupted download stays under
`models/.install-<artifact-id>/<filename>.part` and is resumed on the next
install attempt. Concurrent installers wait on a private lock instead of
downloading twice. Resume responses must contain an exact `Content-Range`; an
invalid range or HTTP 416 removes the partial, and the next install starts
again. A response that would exceed the pinned artifact size is rejected
before the oversized chunk is written.

If a resume repeatedly fails, inspect the model directory listed in
[Privacy](../PRIVACY.md). Preserve and move aside only the affected hidden
`.install-<artifact-id>` staging directory, then retry. Do not replace a
managed filename with an unverified download.

## A Compact, Balanced, or Accurate model will not install

Inspect the catalog and its machine-readable publication state:

```sh
howto model list
howto model list --json
```

The three new tuned profiles are currently present for review but deliberately
unpublished. Their research results are 63.75%, 67.75%, and 74.50% executable
pass@1 respectively on one 400-task Linux/bash fold. The listed disk sizes are
reference quantizations, and any latency or peak-memory figures came from BF16
on an NVIDIA RTX 5090—not final GGUF measurements on a Mac or representative
Linux host.

Installation remains blocked until tuned-derivative redistribution is cleared
and an immutable merged GGUF has an exact URL, size, SHA-256, verified prompt
and stop contract, regression result, and Mac/Linux runtime qualification.
`howto setup --model balanced` and `howto model install balanced` therefore
return the catalog reason before downloading anything. This is not a network
error. Continue using `legacy`; do not work around the gate by substituting an
unreviewed artifact.

## Model verification failed

Run:

```sh
howto model status --deep
howto server stop
```

The initial managed download is verified before installation, but normal
startup uses the pinned artifact's path and exact byte size for a fast check.
`--deep` is the explicit full SHA-256 verification.

If the deep check says `FAILED`, run `howto model install --yes` for the current
legacy release. Explicit managed installation hashes an existing artifact and
replaces it when verification fails. You do not need to remove it manually.

A custom `model_path` or `HOWTO_MODEL` is accepted when it is a regular GGUF
larger than 1 MiB with a `GGUF` header. Stored config paths must be absolute;
environment override paths are canonicalized. HowTo does not know that
model's expected digest.
`--deep` verifies only artifacts matching a published built-in catalog entry.
Verify custom models against their publisher's signed metadata.

## An old config mentions `llama-server`

Current local inference is compiled into `howto`; installing or configuring
`llama-server` is unnecessary. The schema-1 `llama_server_path` setting and
`HOWTO_LLAMA_SERVER` environment variable are deprecated and ignored locally.
You may clean them up after upgrading:

```sh
howto config unset llama_server_path
unset HOWTO_LLAMA_SERVER
howto doctor
```

`howto server status` can report an old HowTo-managed `llama-server` process
left from the previous architecture. `howto server stop` validates ownership
and cleans up that legacy process as well as the current embedded engine.

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
or a newer Bash. For a safe symlink in a startup path, HowTo shows the path, resolved
target, and exact managed block and asks separately before following it; the
default answer is No, and `--yes` does not bypass this review. If setup refuses
an unsafe symlink or malformed startup file, resolve that configuration
manually rather than replacing the file, or complete onboarding explicitly with
`howto setup --no-shell`. Retry integration afterward (substituting `bash` or
`fish` as needed):

```sh
howto shell enable --shell zsh
```

An explicit `howto setup --shell ...` request remains strict and reports the
error.
`howto shell disable` removes the selected or receipt-tracked integration;
`howto setup --no-shell` sweeps every HowTo-managed shell integration.

## Hide the post-query Tab reminder

Keep Tab insertion active but hide only the reminder printed after an eligible
single-command result with:

```sh
howto config set show_tab_hint false
```

The pending command is still available at the next empty prompt. Restore the
default reminder with `howto config unset show_tab_hint` or explicitly set the
value to `true`.

## The failed-command advisor does not appear

The beta advisor is disabled by default. Inspect the preference, provider mode,
and shell integration:

```sh
howto config get failed_command_advisor
howto config get server_url
howto shell status
```

Enable it explicitly with:

```sh
howto config set failed_command_advisor true
```

The managed adapter checks the setting again before sending failed command
text, so enabling or disabling it takes effect on the next eligible failure in
the current shell. If `howto shell status` reports that the shell integration
itself is inactive or out of date, run `howto shell enable` and open a new
terminal.

The advisor requires active shell integration, the embedded local model
(`server_url` must be `null`), and zsh, fish, or Bash 5.1 and newer. Bash
4.1–5.0 supports Tab insertion but not failed-command advice. A configured provider never
receives failed command text or exit status; the advisor remains unavailable
rather than falling back to that provider. The two settings are mutually
exclusive: disable `failed_command_advisor` before setting `server_url`. On
Bash, interactive command history must also be enabled; HowTo reads the current
command from that existing history without creating another one.

Only a command entered at an interactive prompt that exits unsuccessfully can
trigger the advisor. HowTo passes its raw text and exit status to the local
model, so it cannot use command output or inspect the working directory,
environment, or files when forming the suggestion. Local generation may delay
the next prompt by up to 20 seconds. If another terminal is changing the model
engine or that deadline is exhausted, HowTo skips the optional suggestion.

Eligibility is intentionally conservative. The command must be one literal
external invocation no longer than 4096 bytes and exit with status 1–127.
Pipelines, redirects, expansions, wrappers, builtins, control characters,
leading whitespace, and secret-like text are skipped. These privacy filters are
not proof that every sensitive value will be detected.

To turn off automatic suggestions without removing Tab insertion:

```sh
howto config set failed_command_advisor false
```

HowTo never executes, copies, inserts, or stages an advisor suggestion. Review
the printed command as untrusted text before typing or running anything.

## The embedded engine does not start

First stop any recorded engine (and any legacy managed process) and retry:

```sh
howto server stop
howto "show my current directory"
```

HowTo includes the last several engine-log lines in startup errors. The full
log is normally:

- macOS: `~/Library/Logs/HowTo/engine.log`
- Linux: `${XDG_STATE_HOME:-~/.local/state}/howto/log/engine.log`
- `HOWTO_HOME`: `$HOWTO_HOME/logs/engine.log`

If `engine.json` is malformed, HowTo quarantines it beside the original as a
timestamped `engine.json.corrupt-*` file, cleans the related live-state files,
and prints a warning. Preserve that file only if it is useful for a sanitized
bug report.

Common causes are an invalid/incompatible GGUF, insufficient memory, an
accelerator initialization problem, or a startup deadline that is too short.
For a slow machine, increase the deadline within its 1–600 second range:

```sh
howto config set startup_timeout_seconds 180
```

If the log reports a GPU or Metal initialization/context failure, force the
portable CPU path and retry. This preference is fingerprinted, so HowTo
restarts the resident worker with the requested backend:

```sh
howto config set accelerator cpu
```

Restore automatic acceleration later with `howto config unset accelerator`.

The native llama.cpp revision is locked into the HowTo build, so include
`howto --version`, the platform/architecture, selected profile, and the
sanitized engine-log tail in a report. A custom model may require a newer GGUF
architecture or prompt template than that release supports.

## The Unix socket path is too long

Unix sockets have a small operating-system path limit. This most often happens
when `HOWTO_HOME` points into a deeply nested directory. Unset it to use the
normal runtime location, or choose a short, absolute, private path. Values for
`HOWTO_HOME`, Linux XDG directory overrides, and fallback `TMPDIR` are rejected
when relative. Do not point the runtime at a shared directory: it contains
local authentication and process state.

## Server status says stopped

The embedded model engine starts lazily on a query. A stopped result before the first
query is normal:

```sh
howto "show my current directory"
howto server status
```

The embedded worker exits after 15 minutes idle. Use
`howto server stop` when you need to release it immediately. `server status`
describes only the local embedded engine; it does not probe a configured
provider. Status checks serialize with lifecycle changes; normal query reuse
allows a waking engine up to three health attempts before replacing it. If it
reports a legacy `llama-server`, run `howto server stop` once to clean up the
pre-upgrade process.

## A configured provider fails

HowTo expects a llama.cpp-compatible OpenAI-style chat endpoint. Configure
the base URL without `/v1/chat/completions`, because HowTo appends that route:

```sh
howto config set server_url https://provider.example
howto config set model_id provider-model-name
export HOWTO_API_KEY='...'
```

The beta failed-command advisor is intentionally unavailable in this mode and
does not send failed command text or exit status to the configured endpoint.
Configuration rejects `server_url` while `failed_command_advisor` is true, so
disable the advisor first. This does not affect explicit HowTo queries.

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
Current schema 2 includes `model_profile`; schema 1 loads as schema 2 with the
`legacy` profile, while schema 0, future schemas, and unknown keys are refused.
Select profiles with `howto setup --model ...` or `howto model install ...`,
not `config set model_profile`, so the model is verified before selection.
Stored model and shell paths and `HOWTO_HOME` must be absolute. The deprecated
`llama_server_path` is still accepted for migration but ignored locally. HowTo
will not read a symlinked config file or use a data directory owned by another
user.

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
posting. Never include `HOWTO_API_KEY`, the private `engine.key`, or another
secret. Report vulnerabilities privately as described in
[SECURITY.md](../SECURITY.md).
