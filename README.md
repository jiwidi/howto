# HowTo

Turn plain English into a shell command:

```console
$ howto "free port 8080"
lsof -tiTCP:8080 -sTCP:LISTEN | xargs kill
CAUTION:
  - xargs constructs command invocations from input [shell.xargs]
  - the command terminates one or more processes [process.kill]
```

HowTo is a native Rust CLI for macOS, with Linux support. It uses a local
GGUF model through `llama.cpp`, so normal requests stay on your machine. There
is no Python environment, `pip`, account, or hosted API in the default path.
Generated commands are printed for review and are never executed by default.

HowTo is pre-1.0. The command and configuration interfaces are usable, but may
still change before the first stable release.

## Install

Installation is one command; Homebrew adds the tap automatically:

```sh
brew install jiwidi/tap/howto
```

The formula installs the native `howto` executable, `llama.cpp`, a manual page,
and completions for zsh, bash, and fish. It supports
Apple Silicon and Intel Macs. The CI and release workflows also build Linux
arm64 and x86-64 targets.

Tagged releases attach standalone archives named with their Rust target
triples. The `*-unknown-linux-gnu` binaries are built on Ubuntu 22.04 and are
intended for systems with glibc 2.35 or newer; build from source on older or
non-glibc distributions. The `*-apple-darwin` archives are ad-hoc signed for
structural verification but are not Developer ID signed or notarized, so
macOS users should prefer the Homebrew source install. Standalone archives do
not bundle `llama-server` or the model.

Run setup once after installation. It verifies the runtime, downloads the
pinned local model (about 940 MiB), and optionally enables context-aware Tab
insertion for zsh, Bash, or fish. The download can resume and is installed only
after its size and SHA-256 digest match the built-in manifest:

```console
$ howto setup
Runtime found at /opt/homebrew/bin/llama-server.
Download the local model (940 MiB)? [Y/n]
Enable Tab to insert your last generated command at an empty prompt? [Y/n]
HowTo setup is complete.
```

Open a new terminal after enabling the integration. For unattended setup, use:

```sh
howto setup --yes
howto doctor --deep
```

HowTo writes a marked source block to the selected shell's startup file(s) and
stores the embedded adapter in its private data directory. Bash uses both
`~/.bashrc` and the active login file (`.bash_profile`, `.bash_login`, or
`.profile`); zsh and fish use their configured startup locations. Existing
files are backed up before an atomic edit, and the exact managed paths are
recorded so a later disable still cleans them up if shell settings move.

Use `howto setup --no-shell` to complete onboarding without an integration (and
remove any prior HowTo-managed hooks), or `howto setup --shell zsh|bash|fish`
to select one explicitly.
Setup is idempotent and uses an existing verified model or configured provider
without downloading another model. A normal query made before setup offers to
run it interactively; non-interactive queries fail with an instruction instead
of consuming stdin or downloading data unexpectedly.

The Tab hook is optional. Automatic setup finishes without it when the selected
shell is unsupported or symlink approval is declined or unavailable. If a
startup path contains a safe symlink, interactive setup shows the path, resolved
target, and exact managed block in a separate default-No approval prompt;
`--yes` never bypasses that review. An explicit `--shell` request remains
strict and fails outside an interactive terminal instead of following a link.

The model is downloaded from Hugging Face; prompts are not sent there.

## Use

Use the natural-language command directly:

```sh
howto "free port 8080"
howto find files larger than 1GB
howto -n 3 "show folders using the most disk space"
howto --copy "copy my current directory"
howto --json "show which process listens on port 3000"
```

With shell integration enabled, a successful single-command result with risk
`NO_KNOWN_RISK` or `CAUTION` is available once at the next empty prompt. Press
Tab to insert it into the editable command line without submitting it:

```console
$ howto "free port 8080"
lsof -tiTCP:8080 -sTCP:LISTEN | xargs kill
Press Tab at an empty prompt to edit this command.
$ lsof -tiTCP:8080 -sTCP:LISTEN | xargs kill█
```

Tab keeps its normal completion behavior on a nonempty prompt and when no fresh
suggestion is waiting. The pending suggestion is private to that shell session,
expires after ten minutes, and is deleted when inserted. Bash integration uses
the empty-line completion API available in Bash 4.1 and newer; macOS's default
zsh is fully supported, while the legacy system Bash 3.2 is left unchanged.
HowTo never presses Enter—the inserted command remains untrusted text for you
to inspect and edit.

To hide only the post-query `Press Tab at an empty prompt...` reminder while
keeping Tab insertion enabled, change the persisted setting:

```sh
howto config set show_tab_hint false
```

Set it back to `true`, or run `howto config unset show_tab_hint`, to show the
reminder again. This setting does not disable the shell integration or prevent
an eligible generated command from being offered at the next empty prompt.

Run `howto` with no request for an interactive prompt, or pipe a UTF-8 request on
stdin:

```sh
printf '%s\n' 'show my current IP address' | howto
```

Options are parsed only before the first request word. This keeps flag-shaped
text such as `-name` in a natural-language request. Use `--` when the request
itself begins with a hyphen:

```sh
howto -- "--help in tar"
```

A leading management word is reserved under explicit rules: `help` and
`version` dispatch only when alone; `setup` and `doctor` dispatch when alone or
followed by an option; `model`, `server`, and `config` also dispatch when alone,
or when followed by a documented action or option; and `shell` requires an
action or option. Once management parsing is selected, all remaining arguments
are validated strictly. Natural phrases such
as `howto help me find large files`, `howto setup a venv`, and
`howto shell into a container` remain queries. Use `--` to force query parsing
for a management-shaped request, for example
`howto -- "model status for this project"`.

### Query options

| Option | Behavior |
|---|---|
| `-x`, `-e`, `--execute` | Offer to execute the selected command after the required interactive confirmation. Cannot be combined with `--json` or `--quiet`. |
| `-c`, `--copy` | Copy the selected command with `pbcopy` on macOS, or `wl-copy`, `xclip`, or `xsel` on Linux. |
| `-q`, `--quiet` | Print only the first command, and only when no safety rule matched. Cannot be combined with `--json`, `--execute`, or `--count` greater than 1. |
| `-n`, `--count N` | Generate up to 1–8 distinct alternatives. The first is deterministic; later alternatives are sampled. |
| `--json` | Print the request, platform, provider, timing, commands, risk levels, findings, and source spans as JSON. |
| `--timing` | Print generation time in normal text mode. |

The maximum request size is 8192 bytes. Generated responses must reduce to one
physical command line no longer than 4096 bytes.

### Commands

```text
howto setup [-y|--yes] [--no-shell|--shell zsh|bash|fish]
howto doctor [--deep] [--json]
howto model [status [--deep] [--json] | install [-y|--yes]]
howto server [status [--json] | stop [--json]]
howto shell status [--json]
howto shell enable [--shell zsh|bash|fish]
howto shell disable [--shell zsh|bash|fish]
howto shell init zsh|bash|fish
howto config [list [--json] | path | get KEY | set KEY VALUE | unset KEY]
```

`--deep` hashes a matching managed or packaged copy of the approximately
one-gigabyte default model, so it is intentionally opt-in. Custom models have
no built-in expected digest. The `server` commands concern only HowTo's
managed local `llama-server`.

## Safety contract

Model output is untrusted. HowTo validates its shape, parses it with a Bash
syntax tree, applies macOS- or Linux-specific rules, and assigns one of four
results:

| Result | Meaning | With `--execute` |
|---|---|---|
| `NO_KNOWN_RISK` | No current rule matched. This is not proof that the command is safe or correct. | Requires exact confirmation `y`. |
| `CAUTION` | A known side effect or compatibility concern was found. | Requires exact confirmation `RUN`. |
| `DANGER` | A destructive or high-impact pattern was found. | Always blocked. |
| `UNKNOWN` | The parser could not understand enough to decide. | Always blocked. |

The checker covers shell structure, deferred traps, common command-launching
wrappers, and editor command inputs, as well as common filesystem, disk,
process, privilege, persistence, network, credential, version-control,
container, cluster, and infrastructure operations. It is a conservative
backstop, not a sandbox or complete shell verifier. It cannot establish the
semantic correctness of a command, and new or encoded behavior can evade a
static rule set.

HowTo refuses queries, diagnostics, and model, server, or config operations
as root or through `sudo`. When execution is approved, the command still runs
with your normal user's full filesystem, working directory, network, `PATH`,
and most of the caller environment. Shell startup hooks, exported Bash
functions, and dynamic-loader and common language-runtime injection variables
are removed before launch. `HOWTO_API_KEY` is also removed from executed
commands and clipboard helpers, so neither inherits the configured-provider
credential. The default
macOS shell invocation is `/bin/zsh -f -c`; the Linux default is
`/bin/bash --noprofile --norc -c`. Absolute paths to zsh, bash, sh, and dash
are supported through `shell_path`.

Copying is not approval: `--copy` can copy a `CAUTION`, `DANGER`, or `UNKNOWN`
command so that you can inspect or edit it elsewhere. Always review before
pasting or running it. Tab insertion is likewise not approval and deliberately
does not submit the line. `DANGER` and `UNKNOWN` results are not made available
to the Tab integration. See [SECURITY.md](SECURITY.md) for the threat model.

## Local model and runtime

The default model is
[`nl2sh-1.5b-Q4_K_M`](https://huggingface.co/ThorOdinson246/nl2sh-1.5b-Q4_K_M),
a fine-tune of Qwen2.5-Coder-1.5B-Instruct. HowTo pins the artifact URL, byte
length, and SHA-256 digest in source. `howto model status --deep` rechecks a
managed or packaged copy of that default artifact; custom GGUF files do not
have a built-in expected digest.

The managed `llama-server` starts lazily. It listens on a private Unix-domain
socket, uses a random per-launch bearer token, runs with network features and
the Web UI disabled, and is reused across queries. It is configured to sleep
after 15 minutes idle; stop it immediately with:

```sh
howto server stop
```

Licenses, model provenance, training-data qualifications, and runtime notices
are recorded in [Attribution and third-party notices](docs/ATTRIBUTION.md).

## Configuration

Inspect and change settings without editing JSON by hand:

```sh
howto config list
howto config get threads
howto config set threads 4
howto config set show_tab_hint false
howto config unset threads
howto config path
```

| Key | Default | Purpose |
|---|---|---|
| `model_path` | `null` | Use a specific local GGUF file by absolute path. |
| `llama_server_path` | `null` | Use a specific `llama-server` executable by absolute path. |
| `server_url` | `null` | Use a configured llama.cpp-compatible OpenAI-style endpoint instead of the managed local provider. |
| `threads` | available CPUs, clamped to 1–8 | Inference worker threads. Valid range: 1–512. |
| `context_size` | `2048` | Model context size. Valid range: 256–131072. |
| `max_tokens` | `96` | Maximum generated tokens. Valid range: 16–4096 and strictly smaller than `context_size`. |
| `startup_timeout_seconds` | `90` | Managed-server startup deadline. Valid range: 1–600. |
| `show_tab_hint` | `true` | Show the post-query reminder that Tab can insert the pending command. Setting this to `false` hides only the reminder; Tab insertion remains enabled. |
| `shell_path` | `/bin/zsh` on macOS; `/bin/bash` on Linux | Absolute path to zsh, bash, sh, or dash, used only after approved execution. |
| `model_id` | `howto` | Model name sent to the completion endpoint and local runtime alias. |

The configuration file is written atomically with owner-only permissions.
Schema 0, future schemas, and unknown keys are rejected. Environment overrides
are useful for testing and managed deployments. On Linux, any set
`XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME`, or
`XDG_RUNTIME_DIR` value must also be absolute. When runtime state falls back to
the system temporary directory, a set `TMPDIR` must be absolute as well.

| Variable | Effect |
|---|---|
| `HOWTO_HOME` | Put configuration, data, cache, logs, and runtime state under one absolute directory. Relative paths are rejected. |
| `HOWTO_MODEL` | Override `model_path`. |
| `HOWTO_PACKAGED_MODEL` | Add an explicit package-provided default-model candidate. |
| `HOWTO_LLAMA_SERVER` | Override `llama_server_path`. |
| `HOWTO_API_KEY` | Send a bearer token to a configured endpoint; it is not used as the managed local token. |
| `HOWTO_SHELL_SESSION` | Internal per-shell identifier exported by the managed Tab adapter; do not set it manually. |

### Replaceable provider boundary

To use another llama.cpp-compatible OpenAI-style chat-completion server:

```sh
howto config set server_url https://provider.example
howto config set model_id your-model-name
export HOWTO_API_KEY='...'
howto "show the current directory"
```

The URL is a base URL: HowTo appends `/v1/chat/completions`. Supported schemes
are `https://`, `http://`, and `unix:///absolute/socket/path`; HTTP(S) URLs may
have a base path but cannot contain credentials, a query, or a fragment.
Prefer HTTPS or a local Unix socket; plain HTTP exposes prompts and credentials
in transit. The request includes llama.cpp's `repeat_penalty` and
`repeat_last_n` fields, so compatibility with every OpenAI-style service is
not guaranteed. Each requested alternative is a separate `n: 1` completion,
so a remote provider receives the prompt—and the bearer token when set—more
than once.

Configured providers replace only inference. HowTo's output validation,
platform rules, risk display, and execution gate remain local. Requests may
leave the machine, and the provider's retention policy applies. Return to the
managed local provider with:

```sh
howto config unset server_url
howto config unset model_id
```

Read [PRIVACY.md](PRIVACY.md) before configuring a remote endpoint.

## Diagnostics

Start with:

```sh
howto doctor --deep
howto model status --deep
howto server status
```

In configured-provider mode, `howto doctor` makes a `GET /health` request and, if
that is not successful, `GET /v1/models`, using `HOWTO_API_KEY` when set. At
least one route must return a successful status; this checks a supported
diagnostic route, not chat-completion compatibility.

In local mode, `doctor` also runs the resolved `llama-server --version` with a
30-second deadline. Local readiness means the model is present, this runtime
probe succeeds, setup has completed, and an optional deep model check did not
fail; a harmless query is still the end-to-end inference test.

See [Troubleshooting](docs/TROUBLESHOOTING.md) for download, runtime, provider,
clipboard, and execution problems. The implementation is described in
[Architecture](docs/ARCHITECTURE.md).

## Project

- [Security policy and threat model](SECURITY.md)
- [Privacy behavior](PRIVACY.md)
- [Contributing](CONTRIBUTING.md)
- [Changelog](CHANGELOG.md)
- [Reference implementation audit](docs/REFERENCE_AUDIT.md)
- [Locked Rust dependency licenses](THIRD_PARTY_LICENSES.txt)
- [Apache-2.0 license](LICENSE)

HowTo is independent of the projects it builds on; no upstream endorsement is
implied.
