# Security Policy

## Dependency Policy

All dependencies are treated as potential supply chain vectors. Before any
dependency is added:

1. The source MUST be audited for malicious or unexpected behavior
2. `build.rs` scripts MUST be reviewed — they execute arbitrary code at compile time
3. Transitive dependencies MUST be enumerated and reviewed
4. The maintainer identity and project history MUST be assessed

When native libraries (C/C++) are needed, the source is vendored directly from
the upstream project, checksums are verified against the project's published
hashes, and minimal FFI bindings are written in-house. See `vendor/` for
vendored sources and their `PROVENANCE.md` files.

The `cc` crate (maintained by the rust-lang organization) is the sole
build-time dependency used for compiling vendored C code.

## Credential Storage

- All credential files are written with mode `0600` (owner read/write only)
- OAuth tokens, API keys, and session data are stored under `~/.git-ai/internal/`
- Credentials are NEVER logged, included in telemetry payloads, or transmitted
  except to authenticated endpoints over TLS

## Network Security

- All HTTP requests use TLS (HTTPS) by default
- `curl --fail` is used so HTTP error responses (4xx/5xx) are treated as failures
- No `--insecure` or certificate verification bypass is ever used
- Telemetry uploads require authentication (token or API key) unless the user
  has configured a custom `api_base_url`
- Remote URLs are stripped of embedded credentials before any telemetry emission

## Data Boundaries

- Authorship notes contain line ranges, hashes, and metadata — never source code
- CAS objects are capped at 512KB to prevent accidental source code transmission
- Transcript uploads contain event metadata, not file contents
- The offline telemetry queue is bounded (50K rows / 10MB) with FIFO eviction

## Daemon Security

- The daemon runs as the invoking user, never as root
- Unix domain sockets are restricted: directory is `0700`, socket file is `0600`
- The trace2 event listener caps input lines at 256KB to prevent memory exhaustion
- Maximum concurrent connections are bounded (64) to prevent resource exhaustion
- No `setuid`, no privilege escalation, no child process execution except `git` and `curl`

## Input Validation

- Trace2 JSON events are parsed with `serde_json` — malformed input returns None
  and is silently discarded (no panic, no crash)
- Line length limits prevent a malicious local process from exhausting daemon memory
- File paths from external input are never passed to shell interpreters

## Reporting Vulnerabilities

If you discover a security vulnerability, please report it responsibly by
emailing security@usegitai.com. Do not file public issues for security bugs.
