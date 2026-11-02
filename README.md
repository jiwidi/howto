# HowTo

Turn plain English into a shell command:

```console
$ howto "free port 8080"
lsof -tiTCP:8080 -sTCP:LISTEN | xargs kill
CAUTION:
  - xargs constructs command invocations from input [shell.xargs]
  - the command terminates one or more processes [process.kill]
```

HowTo is a native Rust CLI for macOS, with Linux support. Its installed binary
contains the llama.cpp inference runtime and loads a local GGUF model, so normal
requests stay on your machine. There is no Python environment, `pip`, separate
model-server install, account, or hosted API in the default path.
Generated commands are printed for review and are never executed by default.

HowTo is pre-1.0. The command and configuration interfaces are usable, but may
still change before the first stable release.

## Install

Installation is one command; Homebrew adds the tap automatically:

```sh
brew install jiwidi/tap/howto
```

The formula installs the self-contained native `howto` executable, a manual
page, and completions for zsh, bash, and fish. It supports
Apple Silicon and Intel Macs. The CI and release workflows also build Linux
arm64 and x86-64 targets.

Tagged releases attach standalone archives named with their Rust target
triples. The `*-unknown-linux-gnu` binaries are built on Ubuntu 22.04 and are
intended for systems with glibc 2.35 or newer; build from source on older or
non-glibc distributions. The `*-apple-darwin` archives are ad-hoc signed for
structural verification but are not Developer ID signed or notarized, so
macOS users should prefer the Homebrew install. Standalone archives contain the
native engine but do not bundle a model.

Run setup once after installation. It chooses or preserves a model profile,
downloads its pinned local artifact, and optionally enables shell integration
for zsh, Bash, or fish. A download can resume and is installed only after its
size and SHA-256 digest match the built-in catalog:

```console
$ howto setup
The new Compact, Balanced, and Accurate profiles are awaiting final artifact qualification; using the pinned legacy model for this setup.
Embedded local inference is included with HowTo; no separate runtime is needed.
Download Legacy — nl2sh 1.5B (940 MiB)? [Y/n]
Enable Tab to insert your last generated command at an empty prompt? [Y/n]
Enable the beta failed-command advisor? It sends only raw failed-command text and its exit status to the embedded local model, then prints a suggestion without running it. [y/N]
HowTo setup is complete.
```

Open a new terminal after enabling the integration. For unattended setup, use:

```sh
howto setup --yes
howto doctor --deep
```

`--yes` accepts the default-Yes setup prompts, but it never opts in to the
failed-command advisor or approves a shell startup symlink. Non-interactive
setup leaves the advisor disabled; enable it later with an explicit config
command if desired.

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
without downloading another model. Existing schema-1 installations migrate to
the `legacy` profile, preserving the model and behavior they already approved.
A normal query made before setup offers to
run it interactively; non-interactive queries fail with an instruction instead
of consuming stdin or downloading data unexpectedly.

The Tab hook is optional. Automatic setup finishes without it when the selected
shell is unsupported or symlink approval is declined or unavailable. If a
startup path contains a safe symlink, interactive setup shows the path, resolved
target, and exact managed block in a separate default-No approval prompt;
`--yes` never bypasses that review. An explicit `--shell` request remains
strict and fails outside an interactive terminal instead of following a link.

After compatible shell integration is successfully configured, interactive
setup separately offers the beta failed-command advisor with a default answer
of No. The advisor is available only with the embedded local model, and only for
zsh, fish, or Bash 5.1 and newer. It is not offered when `server_url` selects a
configured provider.

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

### Beta failed-command advisor

The failed-command advisor is disabled by default and requires active shell
integration. When explicitly enabled, it observes a command entered at an
interactive prompt. If that command fails, HowTo sends only its raw command
text and numeric exit status to the embedded local model and prints a suggested
replacement. This can delay the next prompt while the local model responds;
engine acquisition and inference share a 20-second overall deadline, and a busy
engine causes that suggestion to be skipped.

```console
HowTo beta suggestion (review only; not executed):
  git status
```

The advisor does not capture command output, the current directory, environment
variables, directory listings, or file contents. HowTo does not keep a failed-
command history. Suggestions are untrusted text: the advisor never executes,
copies, inserts, or stages one for the Tab shortcut.

Enable or disable it explicitly:

```sh
howto config set failed_command_advisor true
howto config set failed_command_advisor false
```

The adapter checks this preference before sending any failed command text, so
changes take effect on the next eligible failure in an already-open shell.
The adapter also binds HowTo's executable and state/model selection when the
shell starts; if you intentionally change `HOWTO_HOME`, XDG paths, model, or
runtime overrides, open a new terminal to activate that new boundary.

The feature supports zsh and fish, plus Bash 5.1 and newer. Bash 4.1–5.0 can
still use Tab insertion but not the advisor. `failed_command_advisor=true` and
`server_url` cannot be configured together, so a configured provider never
receives advisor input; normal explicit HowTo queries continue to use that
provider as documented below. The Bash advisor also requires interactive
command history to be enabled; HowTo reads the current line but does not create
another history.

Eligibility is deliberately narrow: one literal external command of at most
4096 bytes with an exit status from 1 through 127. Shell pipelines, redirects,
expansions, wrappers and builtins, control characters, leading whitespace, and
secret-like text are skipped before inference. These filters reduce accidental
disclosure but cannot recognize every sensitive value.

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
howto setup [-y|--yes] [--model compact|balanced|accurate] [--no-shell|--shell zsh|bash|fish]
howto doctor [--deep] [--json]
howto model [status [--deep] [--json] | list [--json] | install [PROFILE] [-y|--yes]]
howto server [status [--json] | stop [--json]]
howto shell status [--json]
howto shell enable [--shell zsh|bash|fish]
howto shell disable [--shell zsh|bash|fish]
howto shell init zsh|bash|fish
howto config [list [--json] | path | get KEY | set KEY VALUE | unset KEY]
```

`--deep` hashes a matching managed or packaged model, so it is intentionally
opt-in. Custom models have no built-in expected digest. The historically named
`server` commands inspect or stop HowTo's resident embedded engine. They do not
control a configured `server_url` endpoint.

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

## Local models and native engine

Existing installations retain
[`nl2sh-1.5b-Q4_K_M`](https://huggingface.co/ThorOdinson246/nl2sh-1.5b-Q4_K_M),
a fine-tune of Qwen2.5-Coder-1.5B-Instruct, as the `legacy` profile. HowTo pins
its artifact URL, byte length, and SHA-256 digest in source. The command
`howto model status --deep` rechecks a managed or packaged artifact; custom
GGUF files do not have a built-in expected digest.

The same `howto` executable re-launches itself as a private resident worker and
loads llama.cpp in-process. The short-lived CLI talks to it over an owner-only,
authenticated Unix-domain socket using a length-framed protocol. Local mode
opens no TCP port, exposes no HTTP API or Web UI, and needs no `llama-server`
binary. The worker exits after 15 minutes idle; stop it immediately with:

```sh
howto server stop
```

`howto server status` calls this process the **embedded engine**. During an
upgrade it also detects an old HowTo-managed `llama-server`; `server stop`
safely cleans up both recorded process types.

### Model profiles

`howto model list` shows the selected legacy entry plus the next catalog
profiles. The quality results below
are executable pass@1 from one 400-task held-out Linux/bash fold. Disk is the
current quantized reference base; peak memory and latency were measured in BF16
on an NVIDIA RTX 5090. They are research comparisons, not claims about the
final GGUFs or performance on a Mac.

| Profile | Model | Research quality | Reference disk | Research BF16 mean latency | Research BF16 peak | Provisional host RAM |
|---|---|---:|---:|---:|---:|---:|
| `compact` | Qwen2.5-Coder 0.5B | 63.75% | 469 MiB | 86.4 ms | 1.14 GB | 1.5 GiB |
| `balanced` | Qwen3.5 0.8B | 67.75% | 537 MiB | 180.0 ms | 1.62 GB | 2 GiB |
| `accurate` | Qwen3.5 2B | 74.50% | 1,332 MiB | 181.8 ms | 3.96 GB | 4 GiB |

These tuned derivatives are currently **unpublished and not installable**.
Their base models are Apache-2.0, but redistribution clearance for the tuned
artifacts and final immutable merged-GGUF qualification are still required.
The built-in catalog deliberately contains no mutable placeholder download.
Until an artifact has an immutable URL, exact byte count and SHA-256, cleared
distribution status, and Mac/Linux runtime results, setup keeps new and
upgraded users on the pinned legacy model.

Once qualified entries are activated, a new interactive setup will offer the
three choices, with `balanced` recommended. Automation can select one with
`howto setup --model balanced --yes`, and an installed setup can switch with
`howto model install accurate`. Today those explicit commands fail closed with
the catalog's qualification reason; `howto model list --json` exposes the same
status for tooling. `model_path` and `HOWTO_MODEL` remain expert overrides and
bypass profile selection.

Licenses, model provenance, training-data qualifications, and runtime notices
are recorded in [Attribution and third-party notices](docs/ATTRIBUTION.md).

## Configuration

Inspect and change settings without editing JSON by hand:

```sh
howto config list
howto config get threads
howto config set threads 4
howto config set accelerator cpu
howto config set show_tab_hint false
howto config set failed_command_advisor true
howto config unset threads
howto config path
```

| Key | Default | Purpose |
|---|---|---|
| `model_profile` | `legacy` for upgrades; `balanced` after qualified profiles activate for new setup | Selected built-in catalog profile. Readable in config, but changed through `setup --model` or `model install PROFILE` so selection cannot precede verification. |
| `model_path` | `null` | Use a specific local GGUF file by absolute path. |
| `llama_server_path` | `null` | Deprecated schema-1 compatibility field. Ignored by local inference. |
| `server_url` | `null` | Use a configured llama.cpp-compatible OpenAI-style endpoint instead of the embedded local engine. |
| `threads` | available CPUs, clamped to 1–8 | Inference worker threads. Valid range: 1–512. |
| `context_size` | `2048` | Model context size. Valid range: 256–131072. |
| `max_tokens` | `96` | Maximum generated tokens. Valid range: 16–4096 and strictly smaller than `context_size`. |
| `startup_timeout_seconds` | `90` | Embedded-engine startup/model-load deadline. Valid range: 1–600. |
| `accelerator` | `auto` | Use native acceleration when available. Set `cpu` to bypass an unavailable or broken GPU/Metal backend. |
| `show_tab_hint` | `true` | Show the post-query reminder that Tab can insert the pending command. Setting this to `false` hides only the reminder; Tab insertion remains enabled. |
| `failed_command_advisor` | `false` | Opt in to beta, print-only suggestions after eligible failed interactive commands. Uses only the embedded local model and cannot be combined with `server_url`. |
| `shell_path` | `/bin/zsh` on macOS; `/bin/bash` on Linux | Absolute path to zsh, bash, sh, or dash, used only after approved execution. |
| `model_id` | `howto` | Model name sent to the completion endpoint and local runtime alias. |

The configuration file is written atomically with owner-only permissions.
Current configuration schema 2 adds `model_profile`. Schema 1 is migrated in
memory to schema 2 with `legacy`; schema 0, future schemas, and unknown keys are
rejected. Environment overrides
are useful for testing and managed deployments. On Linux, any set
`XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME`, or
`XDG_RUNTIME_DIR` value must also be absolute. When runtime state falls back to
the system temporary directory, a set `TMPDIR` must be absolute as well.

| Variable | Effect |
|---|---|
| `HOWTO_HOME` | Put configuration, data, cache, logs, and runtime state under one absolute directory. Relative paths are rejected. |
| `HOWTO_MODEL` | Override `model_path`. |
| `HOWTO_PACKAGED_MODEL` | Add an explicit package-provided default-model candidate. |
| `HOWTO_LLAMA_SERVER` | Deprecated and ignored by local inference; retained only so old shell integrations/configuration can be upgraded without redirecting the embedded engine. |
| `HOWTO_API_KEY` | Send a bearer token to a configured endpoint; it is not used as the embedded engine's private IPC key. |
| `HOWTO_SHELL_SESSION` | Internal per-shell identifier exported by the managed shell adapter; do not set it manually. |

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

Configured providers are never used by the beta failed-command advisor. While
`server_url` is set, enabling the advisor is rejected. Likewise, set
`failed_command_advisor` to `false` before configuring `server_url`. This keeps
failed command text and exit status away from configured endpoints.

Configured providers replace only inference. HowTo's output validation,
platform rules, risk display, and execution gate remain local. Requests may
leave the machine, and the provider's retention policy applies. Return to the
embedded local provider with:

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

In local mode, `doctor` reports that the runtime is embedded; it does not look
for or probe `llama-server`. Local readiness means setup is complete, the
selected model is present, and an optional deep model check did not fail. The
resident engine starts lazily, so a harmless query remains the end-to-end model
load and inference test.

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
