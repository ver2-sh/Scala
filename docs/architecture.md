# Architecture

Norted Server has two first-class interfaces over one application core. The interactive TUI and scriptable CLI both read the same model registry, configuration, and runtime state. The API gateway receives that same shared core when it starts.

```text
CLI / TUI ───────────────┐
                         ▼
                  norted-core
             config · models · state
                events · provenance
                 ▲              ▲
                 │              │
          norted-engine     norted-api
          adapter contract  public gateway
                 │
       future managed processes
```

## Crate boundaries

`norted-server` is the composition root. It parses commands, initializes structured logging, constructs shared services, launches the TUI or API, handles shutdown, and translates failures into user-facing messages.

`norted-core` owns engine-neutral domain behavior: platform paths, versioned TOML configuration, artifact discovery, shared server state, application events, identifiers, and provenance. It has no terminal dependency. Model discovery recognizes format from `.gguf` and `.q27` extensions only; unknown artifacts are not runnable entries. Full metadata and content hashing are deliberately deferred and never performed during rendering.

`norted-engine` defines the adapter boundary. Identity, upstream origin, supported artifact/API capabilities, install/build provenance, native option metadata, launch specifications, process descriptors, health, and lifecycle operations are represented without importing llama.cpp- or Q27-specific concepts into the common contract. Adapter-specific configuration can remain namespaced, including arbitrary native arguments and environment variables. The registry is honestly empty in this bootstrap.

`norted-api` owns the public protocol and server lifecycle. It currently implements only health and model listing. The future canonical path is OpenAI Responses request → Norted Responses/Item representation → capability and routing layer → selected adapter. Chat Completions can later translate into that canonical representation. Engine-native wire quirks do not belong in this crate.

`norted-tui` owns terminal lifecycle, UI-local state, command definitions, event/update handling, design tokens, overlays, responsive layout, and rendering. Crossterm events and core events enter one update loop; screens receive an immutable application snapshot. Domain work is not performed by rendering components. A restoration guard owns raw mode, alternate screen, cursor visibility, bracketed paste, enhanced-key flags, ordinary exit, error exit, and panic cleanup.

## Process supervision and provenance

Future adapters will turn a normalized launch request plus namespaced native options into a process launch specification. A supervisor boundary will own child process lifetime and health. Runtime provenance can attribute a process to model identity and optional content hash, exact engine version/revision, build provenance, runtime profile, and process ID:

```text
artifact → identity/hash → engine revision → profile → process
```

Neither adapter implementation nor process supervision is claimed by this bootstrap.

## Event flow

The core exposes a broadcast channel for model registry refreshes, server state changes, and application log events. The TUI consumes this channel alongside terminal events and redraw ticks. The same path can accept future engine, request, and process lifecycle events without coupling those producers to Ratatui.

## Configuration and safety

Configuration defaults to `127.0.0.1:8742`, never a public bind address. Platform-native config, data, state, cache, and log directories are distinct. Existing configuration is read but never overwritten automatically. No secrets are included in defaults or documentation.
