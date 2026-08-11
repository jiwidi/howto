# Security policy

HowTo turns untrusted model output into shell text. Its safety design reduces
risk, but it does not make arbitrary generated commands safe. Review every
command as if it came from an untrusted person.

## Supported versions

HowTo is currently pre-1.0 and has no stable release line. Security fixes are
made on `main`. After tagged releases begin, the latest release and `main` will
receive fixes; older pre-1.0 versions may require upgrading.

## Report a vulnerability

If the repository's **Report a vulnerability** button is available, use
GitHub's private vulnerability-reporting flow:

<https://github.com/jiwidi/howto/security/advisories/new>

Include the affected version or commit, platform and architecture, impact,
reproduction steps, and any proposed mitigation. Do not include secrets,
personal prompts, model files, or an exploit for other users' systems.

If that flow is unavailable, open a minimal issue asking for a private
contact channel. Do not publish exploit details in the issue. The maintainers
will acknowledge a complete report, assess severity, coordinate a fix and
release, and credit the reporter when requested and appropriate.

## Security model

HowTo treats four inputs as untrusted:

1. the natural-language request;
2. output from the model or a configured provider;
3. downloaded model and runtime artifacts; and
4. local configuration and runtime state that may have been tampered with.

The default path applies the following controls:

- no generated command executes unless `--execute` was explicitly requested;
- execution requires an interactive, exact confirmation;
- `DANGER` and `UNKNOWN` commands are never executed by HowTo;
- `--quiet` emits only a `NO_KNOWN_RISK` result;
- model output containing terminal control bytes, multiple lines, malformed
  fences, excessive length, or a token-limit finish is rejected;
- a tree-sitter Bash parse and platform-specific rules inspect nested shell
  structure, deferred traps, common launch wrappers, editor command inputs,
  and known high-impact operations;
- queries, diagnostics, and model, server, or config operations refuse root,
  set-user-ID-style elevation, and `sudo howto ...`;
- the managed model artifact is pinned by immutable URL, exact byte count, and
  SHA-256, and is moved into place only after verification;
- downloads are resumable, range-checked, space-checked, locked, and protected
  against writing through symlinks;
- local directories are owner-only, and config, lock, state, key, and log
  files are created with restrictive permissions and symlink checks;
- the managed server uses an authenticated Unix socket, disables proxy use for
  local requests, and starts `llama-server` with `--offline` and `--no-webui`;
- concurrent starts are serialized, state is fingerprinted to the HowTo
  version, managed-launch policy schema, model, runtime, and relevant settings,
  and a PID is signaled only after its command line matches the recorded model,
  server, and Unix-socket path; and
- provider responses are size-limited and must match the expected
  OpenAI-compatible response shape; provider-supplied API error text is capped
  at 2048 bytes and control or bidirectional-format characters are escaped
  before terminal display.

These controls are defense in depth, not a security boundary around the shell.

## What the controls do not guarantee

`NO_KNOWN_RISK` means only that no current rule matched. It does not mean
“safe,” “read-only,” “correct,” or “what the user intended.” Natural-language
models can invert polarity, invent paths or identifiers, select an overly
broad scope, or return a valid-looking command with an unforeseen effect.

The checker is a static policy over a Turing-complete language. Aliases,
program-specific semantics, encoded payloads, dynamic expansion, newly added
tools, shell bugs, and behavior hidden inside scripts or interpreters cannot
all be proven. Some constructs deliberately become `UNKNOWN` and are blocked,
but the absence of an `UNKNOWN` result is not a proof.

When approved, a command runs as the current user and inherits HowTo's
working directory, network access, standard streams, `PATH`, and most of the
caller's environment. HowTo removes shell startup hooks, exported Bash
functions, dynamic-loader variables, common language-runtime injection
variables, and `HOWTO_API_KEY` first. The configured-provider credential is
therefore unavailable to the approved child command or a clipboard helper.
The default zsh and bash
invocations also disable startup files; these measures do not restrict
operating-system permissions. HowTo is not a sandbox, container, privilege
separator, or transaction system, and it cannot undo a command.

Displaying or copying a command is not execution, but it is also not a safety
approval. `--copy` intentionally allows all risk classes so users can inspect
or edit the output. Pasting that command into a shell bypasses HowTo's
execution gate.

## Provider and transport security

Setting `server_url` changes the trust and privacy boundary. HowTo sends the
request and its system prompt to that endpoint and trusts it to implement
`POST /v1/chat/completions`. A `HOWTO_API_KEY`, when present, is sent as a
bearer token.

- Prefer `https://` for remote endpoints and
  `unix:///absolute/socket/path` for same-host endpoints.
- HowTo rejects HTTP(S) endpoint URLs containing embedded credentials, a
  query, or a fragment; do not try to encode secrets in the URL.
- Do not send credentials over `http://`; both prompts and bearer tokens are
  cleartext to the transport path.
- Treat provider output as hostile even if the provider is authenticated.
- Use a narrowly scoped API key and inject it through the environment. Do not
  put it in `server_url`, the JSON configuration, issue reports, or shell
  transcripts.
- A configured custom GGUF is checked only for a plausible file shape, not
  against HowTo's default-model digest. Its provenance is the operator's
  responsibility.

In configured-provider mode, `howto doctor` sends `GET /health` and, if needed,
`GET /v1/models` probes with five-second deadlines, using `HOWTO_API_KEY` when
set. Readiness requires a successful response from either route. These probes
do not verify `POST /v1/chat/completions`; a query is still the end-to-end
compatibility check.

## Artifact and release integrity

Prefer a stable Homebrew formula after one is published. It is generated from
an immutable source release asset with a SHA-256 checksum. A `--HEAD` install
builds mutable `main` and is intended for pre-release testing.

Tagged GitHub releases are designed to include checksums for source and native
archives plus GitHub build-provenance attestations. The workflow stages a
draft release until all assets are present, publishes it so Homebrew can fetch
the immutable source URL, then audits and installs the generated versioned
formula on macOS and Linux. A failed or cancelled Homebrew validation returns
the release to draft automatically. When opt-in tap publishing is enabled, a
stable update is proposed only for non-prerelease tags and only after those
checks. The managed model has a separate pinned digest that can be checked
with:

```sh
howto model status --deep
```

Release tags must point to commits contained in `main`. Native GNU/Linux
archives use the Ubuntu 22.04 glibc baseline. Native macOS archives receive
only the platform's linker/ad-hoc signature and are not Developer ID signed or
notarized; prefer the Homebrew source install unless you independently review
and trust a standalone archive. Checksums and provenance attestations establish
artifact identity and build origin, not Apple notarization or runtime safety.

Homebrew supplies `llama.cpp` as a separate dependency. Its formula, binaries,
updates, and native model-loading attack surface are outside HowTo's static
safety parser. Keep Homebrew, `llama.cpp`, and HowTo current.

## Safer operating practices

- Generate first; do not start with `--execute` for an unfamiliar task.
- Verify paths, hosts, branches, namespaces, devices, process IDs, and wildcard
  expansion against the real environment.
- Prefer a disposable account, VM, container, or test repository when the
  requested operation is inherently destructive.
- Inspect scripts and downloaded content before running them.
- Never build automation around `eval "$(howto ...)"` or equivalent implicit
  execution. `--quiet` is a formatting mode, not a safe-eval API.
- Stop and reformulate when the request or output is ambiguous.

For model-specific evidence and known limitations, see
[the reference audit](docs/REFERENCE_AUDIT.md).
