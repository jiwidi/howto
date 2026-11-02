# Architecture

HowTo is a native, synchronous Rust CLI. Its default architecture keeps model
inference on the user's machine while treating every generated command as
untrusted data until after validation, policy assessment, display, and an
optional interactive execution gate.

## Query flow

```text
request
  │
  ▼
CLI parser ──► setup receipt gate ──► platform prompt + config
  │
  ├── local provider: model resolver ─► managed llama-server ─┐
  │                                                          │
  └── configured provider: HTTP(S) or Unix socket ───────────┤
                                                             ▼
                                                   chat completion
                                                             │
                                                             ▼
                     output-shape validation ◄── strict retry once
                                                             │
                                                             ▼
                                          tree-sitter safety assessment
                                                             │
                                       ┌─────────────────────┼──────────────┐
                                       ▼                     ▼              ▼
                                  text / JSON             clipboard    execution gate
                                                                             │
                                                                  zsh -f / bash --noprofile
```

The ordering is an invariant: sanitize and validate before policy analysis,
and analyze before execution is considered. Output remains untrusted after it
passes the checker.

## Source map

| Module | Responsibility |
|---|---|
| `src/main.rs` | Thin process entry point, stderr error reporting, and exit status. |
| `src/cli.rs` | Dependency-free argument grammar, front-only query options, help, and subcommands. |
| `src/app.rs` | Use-case orchestration, stdout/stderr shape, selection, risk display, query and advisor prompts, diagnostics, and config commands. |
| `src/config.rs` | Versioned JSON schema, defaults, validation, atomic owner-only writes, and symlink refusal. |
| `src/setup.rs` | Versioned setup receipt, setup serialization lock, atomic writes, and incomplete-state detection. |
| `src/shell.rs` | zsh/Bash/fish adapter installation, startup-file management, private pending-command transfer, and failed-command advisor dispatch. |
| `src/paths.rs` | macOS-native, Linux XDG, runtime, and `HOWTO_HOME` paths with owner-only directories. |
| `src/model.rs` | Model manifest, resolution precedence, resumable download, disk-space and range checks, locking, SHA-256 verification, and atomic install. |
| `src/runtime.rs` | `llama-server` resolution, local process lifecycle, Unix-socket HTTP client, authentication, state identity, health, logs, and configured connections. |
| `src/platform.rs` | macOS/zsh/BSD and Linux/bash/GNU generation contracts. |
| `src/inference.rs` | OpenAI-style request construction, deterministic first choice, sampled alternatives, response limits, and decoding. |
| `src/extract.rs` | Removal of permitted wrappers and rejection of multiline, control-byte, malformed, oversized, empty, or truncated output. |
| `src/safety.rs` | Tree-sitter traversal, literal decoding, wrapper and nested-shell handling, platform compatibility policy, findings, and execution gates. |
| `src/execute.rs` | Interactive exact confirmations, profile-free shell invocation, exit propagation, and platform clipboard adapters. |
| `src/error.rs` | Error taxonomy and stable process exit-code classes. |

`src/lib.rs` exposes the modules to integration tests and future embedding, but
the supported product surface is the CLI.

## CLI boundary

Query flags are consumed only until the first non-option request word. Natural
language frequently includes shell flags, so parsing the complete argument
list as options would corrupt requests such as `find files with -name`.
`--` explicitly terminates option parsing.

Management dispatch uses the leading argument only. `help` and `version`
dispatch when alone; `setup` and `doctor` dispatch when alone or followed by an
option; `model`, `server`, and `config` also dispatch when alone, or when
followed by one of their documented actions or an option; and `shell` requires
an action or option. Once selected, the management parser validates all
remaining arguments strictly. Ordinary followups such as
`help me`, `setup a venv`, and `shell into a container` remain query text. `--`
forces query parsing when natural language begins with a management-shaped
prefix.

With no argument and a terminal, `app` asks for a request. With non-terminal
stdin, it reads at most 8192 bytes of valid UTF-8. Requests passed as arguments
are joined with spaces and have the same limit.

Commands are written to stdout. Progress, diagnostics, safety findings,
timing, selection, and confirmation use stderr. This permits ordinary capture
without hiding warnings. JSON is a query output format, not an execution
protocol, so the parser rejects `--json` together with `--execute`. Quiet mode
is also incompatible with JSON, execution, and a candidate count greater than
one; it cannot conceal a selection or an undisplayed alternative.

## Setup and shell integration

Query generation requires a separate schema-versioned `setup.json` receipt.
The presence of a model alone is not treated as consent or completed
onboarding. An interactive query without the receipt offers to run the same
setup workflow; JSON, quiet, piped, and other non-interactive requests fail
before consuming request input. Help, version, setup, doctor, model, server,
shell, and config operations remain available for repair.

Setup is serialized with a private lock. After the runtime/model phase and
optional shell phase finish, it writes a recoverable receipt checkpoint before
asking for the independent beta-advisor preference. This keeps startup files
and setup state consistent if the user interrupts that final prompt; rerunning
setup offers the unanswered prompt again. Local setup resolves `llama-server`
before a potentially large download, verifies an existing managed artifact,
and otherwise uses the resumable installer. A configured provider skips local
runtime and model acquisition. Repeated setup repairs the same integration
without adding duplicate startup blocks.

After compatible shell integration is successfully configured, interactive
setup can offer the beta failed-command advisor through a separate default-No
prompt.
Neither `--yes` nor a non-interactive setup implies consent. The preference
defaults to false and remains false when the prompt cannot be shown. A
configured `server_url` makes the advisor unavailable, so setup does not offer
the feature in configured-provider mode.

An unsupported automatically selected shell, declined symlink approval, or an
unattended run that cannot request that approval is non-fatal: setup records
completion without a shell. Explicit `--shell` requests and repairs of an
already recorded integration remain strict so automation and existing state do
not silently diverge. Other startup-file validation or mutation failures remain
fatal and transactional.

A safe symlink in a startup path is resolved read-only before installation. The
interactive application layer displays the path, canonical target, and exact
managed block, then requires a separate default-No approval that `--yes` does
not imply. The shell layer re-resolves the link immediately before writing and
accepts only the exact approved target and block. A recorded canonical target
does not prompt again on an idempotent repair.

The zsh, Bash, and fish adapters are tracked source files embedded into the
binary with `include_str!`. Setup writes the selected adapter to the stable
user data directory and atomically adds a marked source block to the shell's
startup file or files, backing up each existing file first. Bash manages both
`.bashrc` and its active login file. The setup receipt records the exact paths,
so migration and disable remain reliable when `ZDOTDIR`, XDG configuration, or
Bash login-file precedence later changes. No Homebrew Cellar path is persisted.
The current parent shell cannot be changed by a child process, so activation
starts in a new shell. `shell disable` removes the recorded blocks and adapter.

Each loaded adapter exports a non-secret session identifier. After an
interactive single-result query, `NO_KNOWN_RISK` and `CAUTION` commands can be
written to an owner-only runtime file keyed by the SHA-256 of that identifier.
Tab on an empty buffer atomically claims and deletes the file, validates the
payload again, and places it in the shell line editor without submitting.
Pending commands expire after ten minutes and do not cross shell sessions.
Normal Tab completion is preserved on nonempty buffers and when no command is
pending. Bash uses its empty-line completion API and therefore requires Bash
4.1 or newer; legacy macOS Bash is left unchanged, while macOS's default zsh
is supported.

The same adapter can observe failed top-level interactive commands when the
separately opted-in beta advisor is active. zsh and fish support this path;
Bash requires 5.1 or newer even though Tab insertion needs only 4.1. The
adapter performs a silent, no-input readiness check before passing raw command
text and numeric exit status to the application; a disabled preference causes
no failed-command data transfer or inference. The application rechecks consent
before reading the piped command. At activation, the adapter pins the absolute
HowTo executable, session identifier, and environment values that select its
state, model, and managed runtime. Every automatic call runs with that bounded
environment, so a later project-local `PATH`, `HOWTO_HOME`, XDG, model, or
runtime override cannot redirect captured text or pending Tab state. Config
files under the bound state root remain live. The adapter does not capture
command output, current directory, environment, directory listings, or file
contents. Configuration validation makes
`failed_command_advisor=true` and a non-null `server_url` mutually exclusive,
and the application checks the preference again before starting inference.
Eligible input is sent only to the managed `llama-server` over its authenticated
private Unix socket. Runtime acquisition is nonblocking when another operation
holds the server lock, and acquisition plus inference share a 20-second
deadline. The resulting suggestion is printed as untrusted text; it
is never executed, copied, placed in the line editor, or published as pending
Tab state. HowTo does not persist failed-command or advisor history.

The advisor boundary accepts only one literal external command no longer than
4096 bytes and an exit status from 1 through 127. It rejects parse errors,
additional or compound shell structure, redirections, expansions, globbing,
shell wrappers and builtins, control and bidirectional-format characters,
leading whitespace, and common secret-like markers or opaque values. A missing
executable is eligible only with the standard command-not-found status 127;
otherwise the executable must resolve through `PATH`. This heuristic reduces
disclosure but cannot prove that a command contains no sensitive value.

## Model boundary

The built-in manifest fixes:

- model ID `nl2sh-1.5b-q4-k-m`;
- display artifact `nl2sh-1.5b-Q4_K_M.gguf`;
- an immutable Hugging Face download revision;
- exact size `986048000` bytes;
- SHA-256
  `6f8a17a11129a31074c944f4c2602453fafd9de43bdaeb1630a8f511ec820f71`;
  and
- declared license `Apache-2.0`.

Resolution precedence is:

1. `HOWTO_MODEL`;
2. configured `model_path`;
3. `HOWTO_PACKAGED_MODEL`;
4. model files under known installation prefixes;
5. the managed data directory.

Configured models are required to be regular `.gguf` files larger than 1 MiB.
Their digest is not known to HowTo. Packaged and managed default candidates
must also be regular, non-symlink files matching the built-in byte length
during fast resolution.

The installer serializes concurrent downloads with an advisory lock, reserves
128 MiB beyond the remaining artifact bytes, and resumes only when the status
and `Content-Range` exactly match the pinned artifact. An invalid range or HTTP
416 discards the partial for a clean next-run restart, and a chunk that would
exceed the expected size is rejected before it is written. The installer writes
through an `O_NOFOLLOW` partial file, syncs it, verifies the final length and
digest, then renames it into place. A failed digest is removed.

Normal resolution intentionally does not rehash a one-gigabyte artifact.
`howto model status --deep` is the explicit integrity check. The download revision
used in source and the later audited model-repository revision contain the same
filename, size, and verified digest; details are in
[ATTRIBUTION.md](ATTRIBUTION.md).

## Local runtime boundary

`llama-server` resolution precedence is:

1. `HOWTO_LLAMA_SERVER`;
2. configured `llama_server_path`;
3. `PATH`; and
4. known Homebrew and system locations.

The local manager refuses elevated execution and creates the runtime layout
before taking an exclusive startup lock. A server fingerprint covers the
HowTo version, a managed-launch policy schema, canonical model path, model
byte length and modification time, canonical runtime path, thread count,
context size, token limit, and model alias. Changing a fixed launch flag or
managed-server environment policy requires incrementing that policy schema.

An existing process is reused only when:

- state schema, fingerprint, and socket path match;
- the recorded PID is alive;
- its process command includes the recorded server, model, and socket path; and
- authenticated `/health` succeeds over the expected Unix socket.

Ensure and status operations share the same lifecycle lock, and reuse gives a
recently waking idle server up to three health attempts before replacement.

Otherwise the manager stops only a process that still matches its recorded
identity, clears stale socket/key/state files, and starts a replacement.
Startup failure kills and waits for the child, clears state, and includes a
bounded, terminal-safe runtime-log tail in the error. A missing process during SIGTERM or the
SIGKILL fallback is treated as already stopped.

Malformed state JSON is not trusted or silently overwritten. Under the
lifecycle lock it is renamed to a timestamped `server.json.corrupt-*` file,
related live-state files are cleaned, and a warning identifies the quarantined
file before recovery continues.

The managed process receives:

```text
--host <private Unix socket>
--parallel 1
--api-key-file <owner-only random key>
--offline
--no-webui
--sleep-idle-seconds 900
```

It also receives the configured model, context, thread, alias, and prediction
limits. Its environment is cleared and rebuilt from a small locale, temporary
directory, home, and GPU-selection allowlist; `LLAMA_ARG_*`, proxy, and caller
credential variables are not inherited. The client disables proxies for this
socket. State and key files are owner-only; logs rotate to one backup when they
exceed 2 MiB.

Local `doctor` requires the resolved executable to be named `llama-server` and
runs its `--version` mode in the same allowlisted environment with a
ten-second deadline. Local readiness means the model exists, that probe
succeeds, and an optional deep digest check did not fail; it is not a full
inference request.

## Configured provider boundary

When `server_url` is present, model resolution and managed-runtime startup are
skipped. `runtime::Connection` supports:

- `https://` and `http://` base URLs; and
- `unix:///absolute/path` with proxy use disabled.

`HOWTO_API_KEY`, if present, becomes a bearer token. HowTo appends
`/v1/chat/completions` and sends an OpenAI-style two-message request with
llama.cpp generation extensions. The provider must return
`choices[].message.content`; response bodies are capped at 16 MiB. An API
`error.message` is capped at 2048 bytes and has terminal control and
bidirectional-format characters escaped before it is reported.

This is deliberately a narrow replacement boundary: provider discovery,
credential storage, billing, model download, and vendor-specific APIs are out
of scope. Output extraction and safety policy stay inside HowTo for every
provider.

Configured mode is a trust and privacy change. HTTPS is not enforced because
loopback development servers commonly use HTTP, but remote plaintext transport
is unsafe. Configuration allows an HTTP(S) base path but rejects embedded
credentials, a query, or a fragment; Unix-socket paths must be absolute.
`doctor` tries `GET /health` and then, if needed, `GET /v1/models`, using the
configured bearer token when present and a five-second deadline per request.
Either successful status makes configured mode ready. These probes do not
exercise chat completion, so a query remains the end-to-end compatibility
check.

## Generation and extraction

The system message requests exactly one physical command line for the current
platform. The first completion uses temperature 0.0. If extraction fails, one
strict retry asks for a corrected command. Additional candidates, when
requested, use temperature 0.6 and top-p 0.95; invalid or duplicate sampled
choices are ignored without discarding the first valid result. Every attempt
is a separate request with `n: 1`, matching llama-server's single-choice
behavior.

Requests are non-streaming and include newline, `<|im_end|>`, and Markdown
fence stop sequences, a repeat penalty, and the configured token cap. The HTTP
request has a 180-second deadline.

Extraction removes a complete `<think>` region, one recognized shell fence,
known prose preambles, a prompt glyph, or one pair of inline backticks. It
rejects unsafe control characters before transformation and then requires a
single, nonempty physical line within 4096 bytes. A `finish_reason` of
`length` is rejected.

## Safety and execution

The safety layer parses the generated line with tree-sitter's Bash grammar,
including when the execution target on macOS is zsh. The supported one-line
command subset overlaps substantially, but grammar differences are another
reason the checker is not a proof system.

The checker walks commands, pipelines, lists, substitutions, redirects, and
literal nested shells. It unwraps common launch wrappers, reparses deferred
trap and wrapper shell payloads, fails closed on unresolved editor inputs, and
lexically normalizes
literal paths, distinguishes platform-critical roots, and emits findings with
rule IDs, severity, messages, and byte spans. Dynamic or invalid forms become
`UNKNOWN` when the checker cannot establish enough structure.

Overall risk uses the highest matched severity:

```text
DANGER > UNKNOWN > CAUTION > NO_KNOWN_RISK
```

The execution gate is separate from display and copy:

- `NO_KNOWN_RISK` requires exact `y`;
- `CAUTION` requires exact `RUN`;
- `DANGER` and `UNKNOWN` are blocked; and
- every confirmation and multi-candidate selection requires terminal stdin
  and stderr.

Approved commands run synchronously, inherit the current directory, standard
streams, `PATH`, and most of the caller environment, and return their shell
exit code. Shell startup hooks, exported Bash functions, dynamic-loader and
common language-runtime injection variables, and `HOWTO_API_KEY` are removed
before launch. The provider credential is therefore not available to the
generated child command. The default macOS shell is zsh with `-f`; the Linux default is bash with
`--noprofile --norc`. Configuration also accepts absolute executable paths to
sh and dash.

The safety layer is a policy backstop over known syntax and operations. It is
not a sandbox, semantic validator, or capability boundary. See
[SECURITY.md](../SECURITY.md).

## Configuration and paths

The versioned JSON configuration uses schema 1, rejects schema 0, future
schemas, and unknown keys, fills other missing keys from current defaults,
validates ranges, refuses a symlinked file, and replaces it atomically after
syncing both file and parent directory.
`max_tokens` must be strictly smaller than `context_size`. Stored model,
runtime, and shell paths must be absolute. The `show_tab_hint` boolean is a
presentation preference: disabling it suppresses only the post-query reminder,
not pending-command publication or Tab insertion. The
`failed_command_advisor` boolean defaults to false and is an explicit opt-in;
it has no effect without compatible shell integration and the managed local
provider.

macOS follows native Application Support, Caches, and Logs locations. Linux
follows XDG config, data, cache, state, and runtime locations. Without
`XDG_RUNTIME_DIR`, both platforms use a UID-specific directory under the
system temporary directory. `HOWTO_HOME` replaces all locations with a single
absolute root, primarily for tests and managed deployments; relative values
are rejected.

Linux XDG directory overrides and `HOWTO_HOME` must be absolute; relative
values are rejected before creating state. See [PRIVACY.md](../PRIVACY.md) for
exact locations and retention.

## Packaging and release

The repository formula is the source template for stable tap releases and
optional HEAD builds. It builds and installs the Rust binary from source,
depends on Homebrew's `llama.cpp`, and installs the `howto` manual page and
shell completions plus the project documentation, notices, and locked Rust
dependency-license bundle.
Release automation renders a versioned formula with the immutable source-archive
URL and SHA-256.

A version tag must exactly match `Cargo.toml`. CI builds and tests four native
targets, and the tagged commit must be contained in `main`. The release
workflow creates deterministic native archives named with their Rust target
triples and containing the binaries, documentation, legal and dependency
notices, manual page, shell completions, and embedded-adapter source, plus a
deterministic source archive, `SHA256SUMS`, and GitHub build-provenance
attestations. It uploads through a draft release,
refuses to replace a differing existing asset, and publishes only after the
asset set is complete. It then verifies the source checksum and attestation and
runs Homebrew's strict audit, install, and test on macOS and Linux; failure or
cancellation returns the release to draft. For stable tags only, an opt-in job
can propose the verified formula to
`jiwidi/homebrew-tap`; prereleases never update the stable tap.

## Failure model and non-goals

Errors are grouped for automation:

- 1: execution, I/O, or JSON;
- 2: usage;
- 3: configuration, dependency, or model; and
- 4: runtime server, network, provider response, or invalid model output.

An explicitly executed command returns its own exit status. Negative status
commands such as a missing model may return 1 without an internal error.

Current non-goals include Windows, shell-script generation, background command
execution, automatic privilege escalation, automatic remediation, provider
account management, and a sandbox for generated commands.
