# Reference implementation audit

This document records the 2026-08-10 engineering audit of
[`ThorOdinson246/whatisit-nl2sh`](https://github.com/ThorOdinson246/whatisit-nl2sh)
performed before building HowTo. The purpose is to retain the reference's
strong, tested behavior without inheriting assumptions that are unsafe or
incorrect on macOS.

The audited master commit was
[`4f8574f19b310d817568971629c3cb10bc0490cb`](https://github.com/ThorOdinson246/whatisit-nl2sh/tree/4f8574f19b310d817568971629c3cb10bc0490cb),
version 0.2.1. The repository describes itself as beta and was only a few days
old at the time of audit, so its measurements and design are useful evidence,
not a maturity guarantee.

## Verification performed

- Cloned the public repository separately under `/tmp`; no HowTo source was
  modified during the audit.
- Read the CLI, configuration, inference engine, host-context builder, output
  extractor, downloader, safety checker, tests, workflows, license, notice,
  model cards, and release metadata.
- Ran the complete master test suite on Darwin with Python 3.13:
  `164 passed`.
- Ran the safety corpus directly: `304/304 passed`, comprising 148 expected
  DANGER cases and 144 benign cases plus the remaining caution/performance
  assertions.
- Audited the unmerged
  [`feat/safety-ecosystems`](https://github.com/ThorOdinson246/whatisit-nl2sh/tree/3d894a37361f75b4484943e9bd5800cb884b89e3)
  tip. Its complete suite produced `280 passed`, and its aggregate safety corpus
  produced `412/412 passed` (194 DANGER and 206 benign cases plus remaining
  assertions).
- Checked the live Hugging Face metadata and HTTP object headers for both GGUF
  files, including revision, size, and SHA-256.
- Downloaded and inspected both pinned macOS llama.cpp `b10333` runtime archives,
  their Mach-O deployment targets, dynamic libraries, code signatures, and
  command-line capabilities.

The reference CI runs unit tests on Python 3.9 through 3.12 on Linux and Python
3.12 on macOS. It builds and installs wheels and source distributions. It does
not run an end-to-end GGUF inference, real server lifecycle, concurrent cold
start, or Homebrew installation test; the model, server, and network are mocked.

## Executive outcome

Adopt the user-facing contract, output hardening, artifact verification, and
test-driven safety philosophy. Redesign the server lifecycle, macOS command
policy, model registry, and installation flow.

| Area | Decision | Reason |
|---|---|---|
| Front-only option parsing | Adapt | Natural-language requests legitimately contain flag-shaped words. |
| Explicit execution and two safety tiers | Adapt | The CLI must never execute merely because it generated text. |
| Output extraction and terminal sanitization | Adapt | Model output is untrusted before it is displayed, checked, or executed. |
| Greedy-first generation and sampled alternatives | Adapt | Candidate 1 remains the deterministic best guess. |
| Hash-pinned, atomic model installation | Adapt | A partial or changed one-gigabyte artifact must never look installed. |
| Safety regression corpus | Port and extend | It captures real bypasses and false positives; the feature branch is stronger than master. |
| Python/XDG installation layout | Replace | HowTo is a native macOS-first Homebrew CLI. |
| PID and server-state implementation | Replace | It is incorrect on Darwin and races on concurrent starts. |
| Linux-centric safety paths and prompt | Replace | They do not protect or guide a Mac adequately. |
| Fixed model-slot symlink | Replace | It loses the 1.5B artifact and cannot reliably switch back. |

## Behavior worth preserving

### Natural command-line grammar

The reference's [`QueryArgs`](https://github.com/ThorOdinson246/whatisit-nl2sh/blob/4f8574f19b310d817568971629c3cb10bc0490cb/whatisit_pkg/whatisit/cli.py#L623)
consumes known options only before request text. This solves two practical
problems:

- `find files -name '*.log'` must send `-name` to the model rather than reject
  it as an unknown CLI option.
- a word such as `stop` is a subcommand only in position one; `show me how to
  stop a process` remains a natural-language request.

It also honors `--` and warns when a known HowTo-style option appears after
the request and was therefore treated as text. HowTo should preserve these
semantics for `howto`.

### Output and execution contract

The reference keeps generated commands on stdout and diagnostics on stderr,
honors `NO_COLOR`, and flushes before printing a warning about the preceding
command. Its execution path has several sound invariants:

- generation does not execute by default;
- execution requires an explicit option and an interactive confirmation;
- several candidates require an explicit candidate selection;
- a DANGER result is never executed automatically; and
- quiet output refuses to emit a DANGER command, protecting common command
  substitution and piping paths.

HowTo should retain those invariants. It should not copy the reference
documentation's encouragement to use `eval "$(...)"`: a static denylist is
not strong enough to make arbitrary model output safe for implicit evaluation.

### Prompt and decoding behavior

The reference system prompt is intentionally short:

```text
You are a shell command generator. Output exactly one line: a single POSIX/bash
command that accomplishes the user's request. No prose, no markdown fences, no
explanation.
```

Server inference uses an OpenAI-compatible chat completion with a 64-token cap,
temperature 0, repeat penalty 1.08 over the last 64 tokens, and stop sequences
for newline, `<|im_end|>`, and a Markdown fence. Candidate generation first
requests one greedy result; only additional candidates are sampled at
temperature 0.6 and top-p 0.95. Failure to produce sampled alternatives does
not discard the greedy result.

The default model was evaluated with the short prompt, so every macOS-specific
prompt change needs measurement. HowTo nevertheless needs an explicit macOS
contract: zsh-compatible syntax, BSD userland semantics, no `apt`, `systemctl`,
or Linux `ss`, and no invented paths, PIDs, devices, process names, or bundle
identifiers.

The optional host context is disabled in the actual default configuration,
despite a stale engine comment saying it defaults on. The author measured a
benchmark regression from 54.2% to 45.1% when context was enabled. The same
notes explain that the Linux benchmark underrepresents underspecified real
requests. Treat host context as an experiment with a Mac-specific evaluation,
not as an assumed improvement.

### Untrusted-output extraction

[`extract.py`](https://github.com/ThorOdinson246/whatisit-nl2sh/blob/4f8574f19b310d817568971629c3cb10bc0490cb/whatisit_pkg/whatisit/extract.py)
removes ANSI CSI/OSC and other control bytes before output reaches a terminal,
then strips paired reasoning blocks, Markdown fences, prompt glyphs, common
prose preambles, comments, and inline backticks. It returns the first usable
line. The engine also deduplicates candidates and rejects outputs that ended
because of the token limit or exhibit repeated-flag degeneration.

This ordering matters: sanitize, extract, validate, safety-check, display, and
only then consider execution. HowTo should fail closed on invalid or truncated
shell syntax instead of trying to repair it and should remove all unhandled
terminal escape families, not merely familiar color sequences. The resulting
implementation rejects truncated output and treats a malformed syntax tree as
`UNKNOWN`, which permits review or copying but blocks quiet output and
execution.

### Artifact installation

The reference downloader has strong supply-chain and interruption behavior:

- expected sizes and model SHA-256 values are fixed in source;
- downloads land in a `.part` file and appear at the final path only after the
  hash matches;
- interrupted downloads are removed;
- available disk is checked before starting;
- runtime hashes come from the pinned GitHub release metadata; and
- tar traversal, absolute links, and escaping symbolic/hard links are rejected.

The whole llama.cpp directory is installed rather than only `llama-server`, so
the adjacent dynamic libraries remain resolvable. HowTo improves on the
reference by supporting resumable downloads and should also pin the immutable
model revision in the URL, validate the HTTP range response, sync the completed
file, and use a separate immutable artifact manifest.

## Safety system

The master safety checker combines whole-command patterns with tokenized,
path-sensitive inspection. It strips control bytes, decodes common Bash ANSI-C
quoting, uses `shlex`, normalizes long options, unwraps command prefixes such as
`sudo`, `env`, `nohup`, and `xargs`, recursively checks `sh -c` and simple
`eval` strings, tracks `cd`, and distinguishes exact critical targets from
ordinary descendants.

DANGER findings include destructive filesystem operations, block-device and
filesystem formatting, system shutdown, irreversible Git cleanup, crontab and
firewall wipes, credential exfiltration, reverse shells, privilege escalation,
persistence, history/log tampering, storage teardown, and remote-code-execution
shapes. CAUTION findings include sudo, ordinary network contact, unscoped
process killing, credentials printed to the terminal, world-writable modes,
and persistent startup changes.

The pending safety branch is material. It adds namespaced resources that have
no filesystem path to inspect:

- Docker and Podman volumes and delete-everything forms;
- Kubernetes namespaces, all-resource deletes, and persistent storage;
- database drops, truncation, Redis flushes, and unscoped updates/deletes;
- destructive AWS, GCP, Azure, and GitHub CLI scopes;
- force pushes and Terraform/Pulumi destruction; and
- SSH/network service lockout and outward-facing package publication.

Those rules were tested against close benign forms to control false positives.
Port the regression cases as a behavioral specification even if HowTo's
implementation uses a real tree-sitter Bash syntax tree instead of Python
regular expressions.

The checker remains a denylist over a Turing-complete language. Its regular
expression segment splitter does not understand quoting, and arbitrary aliases,
computed substitutions, encoded programs, or Python/Perl one-liners can evade
static analysis. Execution still inherits filesystem, network, and environment
access. The correct product language is “backstop,” never “sandbox” or “safe.”

## macOS gaps to close

The reference has platform-specific runtime downloads but almost no macOS
command policy. A Mac-focused safety suite needs at least:

- critical roots such as `/System`, `/Library`, `/Applications`, `/Users`,
  `/private`, and `/Volumes`;
- whole-home detection for `/Users/<name>` and sensitive `~/Library` locations;
- `/private/etc` aliases and Keychain, SSH, network, and system-configuration
  files;
- destructive `diskutil` erase, partition, APFS container/volume, and zeroing
  operations;
- `security` keychain deletion or secret-printing operations;
- `pfctl` flush/disable, `csrutil`, `spctl`, FileVault, and NVRAM weakening;
- `dscl` and `sysadminctl` account/home deletion;
- destructive `launchctl`, `tmutil`, and Homebrew operations; and
- intent-sensitive process termination by port, including the common
  `lsof -ti tcp:PORT | xargs kill` form.

Generation evaluation must cover BSD/GNU incompatibilities including `sed -i`,
`stat -f` versus `stat -c`, BSD `date`, BSD `find`, `launchctl`, `lsof`,
`open`, `pbcopy`, `mdfind`, and Homebrew paths on both Apple Silicon and Intel.
The reference's 300-task InterCode-ALFA score is Linux-container evidence and
does not establish Mac accuracy.

## Server lifecycle findings

The reference's preferred architecture—a resident local llama.cpp server over
an authenticated Unix-domain socket inside a 0700 directory—is sound. It avoids
reloading roughly one gigabyte on every query and avoids exposing a shared
loopback TCP port. State files are generally created at 0600 with `O_NOFOLLOW`,
and response bodies are capped at 16 MiB.

The implementation itself must not be carried over unchanged:

1. **Stop is broken on macOS.**
   [`_is_our_server`](https://github.com/ThorOdinson246/whatisit-nl2sh/blob/4f8574f19b310d817568971629c3cb10bc0490cb/whatisit_pkg/whatisit/engine.py#L223)
   verifies a PID through `/proc/<pid>/cmdline`. Darwin has no `/proc`, so
   `stop_server` never sends SIGTERM, then removes its state files and leaves an
   orphaned model server.
2. **Doctor misreports Unix-socket servers.** `running_port` returns `None`
   before checking health when no TCP port file exists. A healthy socket-only
   server therefore appears idle.
3. **Cold start is not serialized.** Concurrent first invocations can start two
   servers and race the socket, key, PID, and log files.
4. **Failure cleanup is incomplete.** Startup timeout, interruption, or an
   early server exit can leave stale state or a detached process.
5. **State has no generation identity.** An already-live server is reused
   without proving its executable version, model digest, configuration, or
   process start time. Upgrades and model changes can keep serving stale state.
6. **TCP fallback has a port race.** Selecting a free port by binding and then
   closing it creates a window before llama.cpp binds. The health probe is
   unauthenticated, so a process that wins the port can receive the bearer key
   on the subsequent completion request.
7. **Logs are not rotated.** Launch keys and potentially prompts can accumulate;
   existing log permissions and symlinks receive less hardening than other
   state files.

HowTo should use one advisory startup lock, Unix sockets only on supported
macOS, an authenticated state manifest containing PID/start identity/model and
runtime digests, atomic state replacement, graceful termination with a bounded
wait, cleanup on every failure path, log rotation, and an upgrade/uninstall
strategy. A launchd-managed helper is also viable if the service lifecycle is
kept invisible to normal users.

## Model registry defect in the reference

The reference uses the published 1.5B filename as both the real artifact and a
fixed “current model” slot. Installing the 3B model unlinks that path—deleting
the downloaded 1.5B artifact—and replaces it with a symlink to 3B. A later
`setup --size 1.5b` sees the symlink as an existing 1.5B path and does not switch
or download. This contradicts the README's claim that both models remain on
disk and switching is symmetric.

HowTo must keep immutable artifacts under their real names and record the
selected model separately in configuration or in a distinct `current` link.
Never overload an artifact filename as mutable registry state.

## Runtime compatibility findings

The reference pins llama.cpp build `b10333`. The inspected Apple Silicon and
Intel archives were 11,015,270 and 11,290,712 bytes respectively, included the
required dylibs and MIT `LICENSE`, and contained llama.cpp version 10333
(`08659901c`). Both `llama-server` binaries declare **macOS 13.3** as their
minimum deployment target, but the reference installer does not check the OS
version. The binaries are linker/ad-hoc signed, not Developer ID notarized.

The inspected runtime advertises automatic GPU-layer selection, so the
reference's blanket “CPU-only” description does not necessarily describe
execution on Apple Silicon. HowTo should either use Homebrew's supported
llama.cpp keg or state and test the exact bundled runtime, minimum macOS version,
architecture, signing/notarization behavior, Metal policy, and API compatibility.

## Model evidence and limitations

The verified default artifact is documented in
[ATTRIBUTION.md](ATTRIBUTION.md). The author reports 0.620 pass rate on the
300-task InterCode-ALFA benchmark for 1.5B and 0.657 for 3B. The training and
evaluation pipeline, raw paired outcomes, and research notes are not in the
repository, so those figures were not independently reproduced in this audit.

The model cards contain more important safety context than the main README:

- for the 1.5B model, two annotators reportedly judged unintended destructive
  or corrupt behavior in 11.0% of adversarial-prompt outputs and 2.0% of
  ordinary-prompt outputs; and
- the 3B card reports that roughly 14% of polarity-sensitive requests invert
  intent, such as producing “largest” behavior for “smallest.”

These errors often produce valid-looking commands and therefore bypass a
destructive-command checker. HowTo needs semantic Mac evaluation, visible
review before execution, and product language that makes model fallibility
unmistakable.

## Production acceptance criteria derived from the audit

Before calling HowTo production-ready:

- run deterministic unit and regression suites on Apple Silicon and Intel;
- execute a curated macOS NL-to-command benchmark, including polarity and
  underspecified requests;
- exercise a real GGUF through the exact shipped llama.cpp version;
- test concurrent cold starts, stale state, crash recovery, stop, upgrade, and
  uninstall behavior;
- test model-download interruption, resume, bad ranges, wrong length, wrong
  digest, low disk, and immutable revision URLs;
- test every DANGER rule against a close benign command and every known bypass;
- verify stdout/stderr and exit-code contracts in TTY and non-TTY contexts;
- keep execution interactive, refuse DANGER, and never describe the checker as
  a sandbox;
- include `LICENSE`, `NOTICE`, [ATTRIBUTION.md](ATTRIBUTION.md), and generated
  Rust dependency notices in release artifacts; and
- test a clean `brew install` plus first query without requiring user-facing
  Python or pip steps.

Licensing and provenance obligations from this audit are recorded separately in
[ATTRIBUTION.md](ATTRIBUTION.md).
