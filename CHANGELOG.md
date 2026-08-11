# Changelog

All notable changes to HowTo will be documented here.

This project follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Until
1.0, minor releases may contain breaking interface or configuration changes.

## [Unreleased]

## [0.1.1] - 2026-08-11

### Fixed

- Update Homebrew explicitly before release validation so hosted-runner code
  cannot become incompatible with current bottle metadata.
- Keep the Homebrew smoke test independent of `llama-server` cold-start
  latency, while allowing the user-facing runtime diagnostic up to 30 seconds
  on a cold machine.

## [0.1.0] - 2026-08-11

### Added

- Native Rust `howto` executable.
- macOS-first prompt policy for zsh and Apple's BSD command-line tools, plus a
  GNU/Linux bash policy.
- Natural-language input from arguments, an interactive prompt, or UTF-8
  stdin, with front-only option parsing, natural followups after management
  words such as `help me`, and `--` support for management-shaped request
  prefixes.
- Greedy command generation, optional sampled alternatives, strict response
  extraction, one retry for malformed model output, and JSON output. Every
  alternative uses a separate llama-server-compatible `n: 1` request.
- Syntax-aware, platform-specific command assessment with
  `NO_KNOWN_RISK`, `CAUTION`, `DANGER`, and `UNKNOWN` results, including
  deferred traps, editor command inputs, command-launching wrappers,
  tilde-user roots, and case-insensitive macOS critical paths.
- Explicit interactive execution gates, permanent blocking of `DANGER` and
  `UNKNOWN`, refusal to run operational commands with root or `sudo`
  privileges, and a sanitized child environment that strips the provider key
  and common startup, loader, and language-runtime injection variables from
  executed commands and clipboard helpers.
- Clipboard output, timing, quiet output restricted to `NO_KNOWN_RISK`, and
  structured safety findings with source spans.
- Seamless first-use download of the pinned
  `nl2sh-1.5b-Q4_K_M` model, including available-space checks, resumable range
  requests, concurrent-install locking, atomic placement, and SHA-256
  verification. Resume ranges are checked strictly, rejected offsets discard
  the partial for a clean next-run restart, and oversized chunks are blocked
  before writing. Managed and packaged fast-path candidates cannot be symlinks.
- Managed `llama-server` lifecycle over an authenticated private Unix socket,
  with startup locking, health checks, state fingerprinting, log rotation,
  stale-process validation, bounded shutdown, offline mode, and idle sleep.
  The fingerprint includes the HowTo version and managed-launch policy schema.
- Replaceable llama.cpp-compatible OpenAI-style provider boundary supporting
  HTTPS, HTTP, and Unix-socket base URLs with an optional environment-provided
  bearer token, diagnostic `/health` and `/v1/models` probes, response-size
  limits, and bounded terminal-safe provider error messages.
- Atomic owner-only JSON configuration, macOS-native and Linux XDG paths,
  environment overrides, model and server diagnostics, and machine-readable
  status. Configuration rejects unknown or unsupported schemas, requires
  `max_tokens < context_size`, and requires absolute stored paths plus absolute
  `HOWTO_HOME`, XDG, and runtime `TMPDIR` overrides. Local diagnostics validate
  the resolved runtime with a bounded `llama-server --version` probe, and
  startup errors sanitize and bound runtime log excerpts before terminal
  display.
- Homebrew formula with `llama.cpp` as a dependency, source-formula rendering,
  `howto` manual and shell completions, and installed project and
  third-party license documentation.
- CI for formatting, linting, tests, scheduled RustSec advisory scanning,
  release builds, and CLI smoke tests on Apple Silicon, Intel macOS, Linux
  arm64, and Linux x86-64.
- Tagged-release automation for native and source archives, checksums,
  provenance attestations, draft-first immutable GitHub Releases, post-release
  Homebrew audit/install tests with automatic return to draft on failure, and
  an optional stable-only Homebrew tap update pull request. Native archive
  names use their Rust target triples; GNU/Linux builds target the Ubuntu 22.04
  glibc baseline, while macOS archives are not Developer ID signed or
  notarized.
- Apache-2.0 project licensing, third-party notices, model provenance,
  reference implementation audit, security policy, privacy disclosure,
  architecture, and troubleshooting documentation.

[Unreleased]: https://github.com/jiwidi/howto/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/jiwidi/howto/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/jiwidi/howto/releases/tag/v0.1.0
