# Norted Server

Norted Server is a standalone, terminal-first local language-model server and runtime manager. It provides one engine-neutral control core to a polished interactive TUI, a deterministic CLI, and a stable public HTTP gateway.

This repository is the initial production foundation. It is related to the broader Norted project, but is intentionally a separate product and repository. No implementation code is copied from Norted.

## Current status

The bootstrap currently provides:

- a responsive Ratatui interface with Overview, Models, Engines, Server, Logs, Settings, and Help views;
- a data-driven slash-command bar with completion, keyboard navigation, paste handling, reusable overlays, and compact/minimum-size modes;
- non-blocking discovery of local `.gguf` and `.q27` artifacts from resolved search paths;
- stable, readable model IDs derived from canonical artifact identity with SHA-256;
- cross-process API runtime observation using atomic descriptors and identity-checked health probes;
- explicit server, runtime, engine capability, and provenance types;
- an engine adapter contract with native argument/environment escape hatches;
- a real Axum server exposing `GET /health` and registry-backed `GET /v1/models`;
- structured file logging and bootstrap diagnostics.

Norted Server has **no engine support yet**: it does not download, build, install, or execute inference engines. It exposes no inference endpoint and does not claim full OpenAI API compatibility.

## Run

The default command opens the TUI:

```console
cargo run -p norted-server
cargo run -p norted-server -- tui
```

The scriptable command tree is:

```text
norted-server
├── tui
├── serve
├── status
├── models list
├── engines list
├── config show
└── doctor
```

Use global `--json` with status, list, configuration, diagnostics, and server startup output where machine-readable output is useful.

Start the API gateway with:

```console
cargo run -p norted-server -- serve
curl http://127.0.0.1:8742/health
curl http://127.0.0.1:8742/v1/models
```

The server defaults to loopback only (`127.0.0.1:8742`). A separately launched `status` command or TUI discovers it through runtime descriptors in the state directory and accepts a descriptor only when `/health` returns the matching random instance identity. Stale descriptors from an unclean exit therefore do not report a dead process as healthy.

`GET /v1/models` returns the OpenAI-style list envelope and local model objects with `id`, `object`, `created`, `owned_by`, and nullable `shutdown_date`. `created` is the local artifact's last-modified Unix timestamp. The route is a narrow compatibility surface, not a claim of complete OpenAI API support.

## Configuration

`norted-server config show` prints the exact platform-native configuration path and the resolved TOML. If the file is absent, defaults are used without creating or overwriting it.

```toml
version = 1

[server]
host = "127.0.0.1"
port = 8742

[models]
paths = ["D:/models"]

[tui]
no_color = false
unicode = true
```

Relative model paths resolve against the directory containing `config.toml`, independent of the launch shell's working directory. Absolute paths remain unchanged. The resolved paths are used by discovery, Doctor, CLI output, and TUI Settings.

Configuration, application data, runtime state, cache, and logs use separate operating-system application directories. The supported schema version is currently `1`; unsupported versions and unknown structured keys are rejected. Arbitrary engine-native settings remain namespaced and extensible. `NO_COLOR` disables TUI colour independently of the configuration, while `tui.unicode = false` selects intentional ASCII glyphs.

Building requires Rust 1.88 or newer. `rust-toolchain.toml` pins 1.88.0 for reproducible local and CI behavior while the manifest declares the truthful MSRV.

## Next phases

Future adapters will manage upstream runtimes behind the engine boundary. The OpenAI Responses API remains the planned primary public inference contract, but is not implemented. Public API behavior will remain separate from engine-native JSON, arguments, and environment variables.

See [docs/architecture.md](docs/architecture.md) for boundaries and dependency direction.
