# Changelog

## 0.1.0 — Scala

First public Scala developer release. The application and command are
`scala`, with Scala-native crates, storage, service, API identity and release assets.

- Apache-2.0 project licensing, third-party attribution and bundled MPL sources.
- rustls 0.23.45 resolves RUSTSEC-2026-0285; smartstring's upstream maintenance
  warning remains tracked without suppression.
- Generated PowerShell installation verifies the selected archive's embedded
  SHA-256 before extraction, preserving cargo-dist installation semantics.
- Terminal-first local inference server with an authenticated OpenAI-compatible API.
- Independently installed llama.cpp, NInfer and q27 runtimes, with existing
  capability admission and Model Profile settings precedence.
- Managed model discovery/download/import and optional Wayfinder-backed Scala Link.
- Server-only archives for Linux x64/ARM64, macOS Intel/Apple Silicon and Windows
  x64, shell/PowerShell installers, and generated SHA-256 checksums.
- Native `scala update --check` and explicitly approved `scala update` / `--yes`,
  with cargo-dist receipt ownership and active-session replacement protection.
- Daily nonblocking TUI update checks and a `/update` command, with cached failures
  and machine-readable CLI output. Application updates preserve model/runtime data.

Models, inference runtimes, GPU drivers and credentials are not included. A server
binary does not promise every inference runtime supports the same platform.
Norted-built and equivalent ordinary artifacts retain identical serving semantics.

Public installers and versioned assets are available through `https://ver2.sh/scala/`.
Native macOS/Windows validation and signing readiness remain outside this developer
release's validation scope. See
[release maintenance](docs/releases.md) for verification and publication gates.
