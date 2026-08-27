# Norted Server

Norted Server is a standalone, terminal-first local language-model server and runtime manager. It provides one engine-neutral control core to a polished interactive TUI, a deterministic CLI, and a stable public HTTP gateway.

This repository is the initial production foundation. It is related to the broader Norted project, but is intentionally a separate product and repository. No implementation code is copied from Norted.

## Current status

The bootstrap currently provides:

- a responsive Ratatui interface with Overview, Models, Engines, Server, Logs, Settings, and Help views;
- a data-driven slash-command bar with completion, keyboard navigation, paste handling, reusable overlays, and compact/minimum-size modes;
- discovery of local `.gguf` and `.q27` artifacts from configured search paths;
- explicit server, runtime, engine capability, and provenance types;
- an engine adapter contract with native argument/environment escape hatches;
- a real Axum server exposing `GET /health` and registry-backed `GET /v1/models`;
- structured file logging and bootstrap diagnostics.

Norted Server does **not yet download, build, install, or execute llama.cpp or Q27**. It does not expose inference endpoints yet and never inserts fake model or engine data.

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

The server defaults to loopback only (`127.0.0.1:8742`).

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

Configuration, application data, runtime state, cache, and logs use separate operating-system application directories. `NO_COLOR` disables TUI colour independently of the configuration.

## Next phases

Planned adapters will manage upstream `llama-server` and `q27-server` binaries behind the engine boundary. The latest OpenAI Responses API will become the canonical public inference contract; Chat Completions compatibility will translate into that representation. Public API behavior remains separate from engine-native JSON, arguments, and environment variables.

See [docs/architecture.md](docs/architecture.md) for boundaries and dependency direction.
