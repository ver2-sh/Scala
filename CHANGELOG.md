# Changelog

## 0.1.0 — Scala

The first packaged Norted Server release channel. Scala is the release name;
the executable remains `norted-server` and application paths do not change.

- Terminal-first local inference server with an authenticated OpenAI-compatible API.
- Independently installed llama.cpp, NInfer and q27 runtimes, with existing
  capability admission and Model Profile settings precedence.
- Managed model discovery/download/import and optional Wayfinder-backed Norted Link.
- Server-only archives for Linux x64/ARM64, macOS Intel/Apple Silicon and Windows
  x64, shell/PowerShell installers, and published SHA-256 checksums.

Models, inference runtimes, GPU drivers and credentials are not included. A server
binary does not promise every inference runtime supports the same platform.
Norted-built and equivalent ordinary artifacts retain identical serving semantics.

The repository remains private. Anonymous installation requires an explicitly
approved public release channel. Native macOS/Windows validation and signing
readiness must be reviewed before advertising consumer readiness. See
[release maintenance](docs/releases.md) for verification and publication gates.
