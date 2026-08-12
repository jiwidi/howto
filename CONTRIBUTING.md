# Contributing to HowTo

HowTo is a small native CLI with a large blast radius: its output can become a
shell command. Changes should keep the default path simple while treating
model output, local state, and provider responses as untrusted.

## Before opening a change

- Use an issue for a user-visible design change, new provider contract, model
  replacement, or safety-policy decision.
- Use the private process in [SECURITY.md](SECURITY.md) for vulnerabilities.
- Keep pull requests focused. Avoid combining a safety change, broad
  refactor, dependency refresh, and packaging change.
- Read [Architecture](docs/ARCHITECTURE.md) and the
  [reference audit](docs/REFERENCE_AUDIT.md) before changing inference,
  runtime lifecycle, model installation, or execution behavior.

By contributing, you agree that your contribution is licensed under the
project's [Apache License 2.0](LICENSE).

## Development setup

The repository uses Rust 1.86, pinned in CI. On macOS:

```sh
brew install rust llama.cpp shellcheck actionlint cargo-audit expect fish
git clone https://github.com/jiwidi/howto.git
cd howto
cargo build --locked
cargo test --locked --all-targets --all-features
```

Linux development is supported with Rust 1.86 and a `llama-server` installation
available on `PATH` for real inference tests. Unit and integration tests do not
download the model and do not need a running `llama-server`; integration tests
use a local fake provider over a temporary Unix socket.

Run the complete local quality gate before submitting:

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo audit --file Cargo.lock
bash -n scripts/render-homebrew-formula.sh
shellcheck scripts/render-homebrew-formula.sh completions/howto.bash shell/howto.bash
zsh -n completions/_howto shell/howto.zsh
fish --no-config --no-execute completions/howto.fish shell/howto.fish
expect -f scripts/test-shell-integration.exp
expect -f scripts/test-setup-symlink.exp
python3 scripts/test-tab-hint.py
actionlint -no-color
ruby -c Formula/howto.rb
PYTHONPYCACHEPREFIX=/tmp/howto-pycache python3 -m py_compile \
  scripts/create-release-archive.py \
  scripts/generate-third-party-licenses.py
cargo fetch --locked
scripts/generate-third-party-licenses.py --check
git diff --check
```

If Homebrew is available, also run:

```sh
brew style Formula/howto.rb
```

CI also checks RustSec advisories, formula rendering, deterministic archive
creation, completion syntax, and the generated dependency-license bundle. It
tests release builds on macOS arm64, macOS x86-64, Linux arm64, and Linux
x86-64. A scheduled weekly scan catches newly published advisories even when
the lockfile has not changed. Dependabot can propose Cargo and GitHub Actions
updates, but those changes still require the same review and locked-build
checks.

## Running from source

Use the development binary directly:

```sh
cargo run -- --help
cargo run -- doctor
cargo run -- "show my current directory"
```

Run `cargo run -- setup` before the first query. To isolate
development state from your normal installation, set `HOWTO_HOME` to a short,
absolute, private temporary directory before running the binary. Relative
values are rejected. Keep it short because the managed runtime uses a Unix
socket with an operating-system path limit.

You can test the provider boundary with a local llama.cpp-compatible
OpenAI-style endpoint:

```sh
cargo run -- config set server_url http://127.0.0.1:8080
cargo run -- config set model_id your-model-name
cargo run -- "show my current directory"
cargo run -- config unset server_url
```

Do not use confidential prompts with a development or third-party endpoint.

## Code expectations

- Keep the crate free of `unsafe` Rust; `Cargo.toml` denies it.
- Preserve the stdout/stderr contract: generated commands and JSON go to
  stdout; progress, findings, timing, and confirmations go to stderr.
- Keep query options front-only so flag-shaped natural language remains text.
- Do not execute by default, do not add non-interactive execution, and do not
  make `DANGER` or `UNKNOWN` executable.
- Do not describe `NO_KNOWN_RISK` as safe. It means no current rule matched.
- Keep local inference on an authenticated Unix socket without proxy routing.
- Keep model artifacts immutable, pinned, resumable, and verified before
  installation.
- Use atomic writes, owner-only state, symlink checks, and advisory locks for
  files that affect execution or server ownership.
- Avoid logging prompts, generated commands, provider keys, or local bearer
  tokens.
- Preserve normal Tab completion outside an empty prompt with a fresh pending
  HowTo result, and never make a shell adapter submit the edited line.
- Return typed errors and preserve the documented exit-code classes.

## Tests

Every behavior change needs a test at the narrowest useful level:

- parser and output-shape cases belong next to `src/cli.rs` or
  `src/extract.rs`;
- configuration, path, model, and runtime invariants belong in their module;
- end-to-end CLI/provider and stdout/stderr contracts belong in
  `tests/cli_integration.rs`; and
- setup receipts, startup-file transactions, pending-command isolation, and
  adapter behavior belong in `src/setup.rs`, `src/shell.rs`, and shell PTY
  checks; and
- shell policy belongs in `tests/safety.rs`.

For a new safety rule, include both malicious cases and close benign controls.
Test macOS and Linux policies explicitly when commands or flags differ. Cover
quoting, wrappers, pipelines, substitutions, redirects, dynamic operands, and
the most plausible bypass—not just the obvious spelling.

A false sense of safety is worse than an explicit `UNKNOWN`. Prefer
fail-closed behavior when static analysis cannot establish enough structure.

Changes to model prompts or decoding need a representative macOS and Linux
evaluation, including polarity-sensitive and underspecified requests. A unit
test that checks a prompt string is necessary but not evidence of model
quality.

## Documentation and attribution

Update user-facing documentation with the code that changes it:

- CLI, config, paths, provider, or output changes: `README.md`,
  `docs/TROUBLESHOOTING.md`, and `docs/ARCHITECTURE.md` as applicable;
- security or data-flow changes: `SECURITY.md` and `PRIVACY.md`;
- every user-visible change: `CHANGELOG.md`; and
- model, runtime, copied source, training-data, or license changes:
  `NOTICE`, `docs/ATTRIBUTION.md`, and, for Cargo changes,
  `THIRD_PARTY_LICENSES.txt`.

Source copied or closely adapted from an Apache-2.0 upstream project needs a
prominent modification notice in the affected file in addition to the
project-level notices. Do not remove upstream copyright or attribution text.

Commit `Cargo.lock` when dependencies change, regenerate
`THIRD_PARTY_LICENSES.txt`, and review license and supply-chain impact, not only
whether the build passes.

## Pull requests

A good pull request explains:

1. the user problem and intended behavior;
2. the trust or privacy boundaries affected;
3. tests and manual checks performed;
4. platform differences; and
5. documentation, migration, packaging, and attribution impact.

Screenshots are rarely useful for this CLI. Prefer exact command lines with
secrets and private paths removed, plus captured stdout, stderr, and exit code.
Never attach a real provider key, full runtime log, confidential prompt, or
unreviewed generated command.

## Releases

Maintainers release by pushing a semantic version tag on a commit contained in
protected `main`; the tag must exactly match `Cargo.toml`, for example
`v0.1.0`. The release workflow reruns quality checks, builds four native
targets, creates deterministic source and binary archives, publishes SHA-256
checksums and build-provenance attestations, and verifies the generated formula
with a strict Homebrew audit, install, and test on macOS and Linux. A failed
Homebrew validation returns the release to draft. After successful checks, an
opt-in job can open a generated formula-update pull request in
`jiwidi/homebrew-tap` for a stable tag; prerelease tags do not update the stable
tap.

Before tagging, move completed entries from `[Unreleased]` in
[CHANGELOG.md](CHANGELOG.md), verify a clean Homebrew install, test first-use
model setup and a real local query, and complete the release checklist in
[ATTRIBUTION.md](docs/ATTRIBUTION.md).
