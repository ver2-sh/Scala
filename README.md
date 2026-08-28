# Norted Server

Norted Server is a standalone, terminal-first local language-model server and runtime manager. It is an engine-agnostic control plane: Norted owns engine compatibility, process lifecycle, routing, normalized state, provenance, and its public API, while upstream engines perform inference.

The first working inference slice uses [llama.cpp](https://github.com/ggml-org/llama.cpp). A discovered GGUF model can be loaded into a user-supplied `llama-server` process, called through Norted's `POST /v1/responses` gateway, and unloaded again without exposing the backend process to clients.

This repository is related to the broader Norted project, but is intentionally a separate product and repository. No implementation code is copied from Norted or llama.cpp.

## Current status

The implemented slice provides:

- a real `llama.cpp` adapter for GGUF text generation;
- validation and reuse of an explicitly configured external `llama-server` binary;
- an engine-neutral runtime manager with `Stopped`, `Loading`, `Running`, `Stopping`, and `Failed` states;
- an engine-neutral child-process supervisor that drains logs, detects exits, terminates the backend, and cleans it up when the serving process shuts down;
- one active model backend at a time, with explicit load and unload operations;
- a dynamically allocated `127.0.0.1` backend endpoint that is never used as the client-facing API address;
- a separate loopback-only, bearer-authenticated control listener discovered through the local runtime descriptor;
- a narrow OpenAI Responses-compatible text surface at `POST /v1/responses`, including non-streaming JSON and translated streaming SSE;
- `GET /health` and the existing four-field OpenAI-style `GET /v1/models` list;
- a Ratatui interface that observes real engine/backend state and can load the selected model with Enter or `/load`, then unload it with `u` or `/unload`;
- runtime provenance for the model, configured engine binary, native inputs, process, and private endpoint.

This is deliberately not full OpenAI API compatibility. There is no public Chat Completions endpoint, no multi-model scheduler, and no managed llama.cpp installer or updater. Q27 and NInfer inference are not implemented; only discovered GGUF artifacts can be loaded by the current adapter.

## Configure llama.cpp

Norted does not download or build llama.cpp in this slice. Obtain a suitable `llama-server` binary separately, then place the following schema-version-1 TOML in the platform-native path printed by `norted-server config show`:

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

[engine."llama.cpp"]
enabled = true

[engine."llama.cpp".settings]
binary_path = "C:/tools/llama.cpp/llama-server.exe"
```

Replace the two paths with paths valid on the host. Forward slashes keep the Windows example valid without TOML backslash escaping. Absolute paths are used directly; relative model paths and a relative `binary_path` resolve against the directory containing `config.toml`, not the shell's working directory.

`binary_path` is the only llama.cpp `settings` key currently supported. Optional native arguments and environment variables remain available through the engine namespace:

```toml
[engine."llama.cpp".native]
arguments = ["--ctx-size", "8192"]

[engine."llama.cpp".env]
EXAMPLE_VARIABLE = "value"
```

Native arguments are appended exactly as configured. Norted owns the primary model source and stable alias, private host and port, API prefix and authentication, TLS, embedding/reranking mode, and multi-model router controls. Native arguments or configured environment variables that replace those semantics are rejected. Ordinary context, GPU, cache, sampling, template, and speculative-decoding tuning remains available. The child inherits the serving process's environment after the supervisor removes the corresponding critical llama.cpp variables, then applies allowed configured `[engine."llama.cpp".env]` entries. Provenance records that parent inheritance occurred, records configured environment names and value hashes rather than raw values, and does not snapshot inherited values. Native-argument provenance conservatively retains bare dash-prefixed arguments but redacts non-option values and the value in `--name=value` forms.

Each time the adapter is asked to probe, it canonicalizes the configured file, runs `llama-server --version` and `llama-server --help`, verifies that `--model`, `--alias`, `--host`, and `--port` are advertised, and hashes the binary with SHA-256. Probe results are not cached in the adapter, so manager initialization and later load checks revalidate the current binary. Version and revision are recorded only when the binary reports them. `norted-server engines list` shows the latest stored or locally obtained installed, disabled, or invalid probe state. The acquisition method is recorded truthfully as an external binary; its source repository remains unverified rather than being inferred from the adapter identity.

## Run, load, and unload

Start the serving control plane in one terminal:

```console
cargo run -p norted-server -- serve
```

The server completes model discovery before it binds. In another terminal, list stable model IDs and load a GGUF model by ID:

```console
cargo run -p norted-server -- models list
cargo run -p norted-server -- load <MODEL_ID>
cargo run -p norted-server -- status
```

`load` contacts the already-running `serve` process; it never launches a hidden second server. Norted resolves the exact discovered artifact, chooses the compatible installed engine, starts `llama-server` with the stable model ID as its alias, waits for `/health`, and then reads and validates the effective generation defaults from `GET /props` before returning `Running`. Startup is bounded to five minutes. If another model is already active, the request fails instead of replacing it.

Unload the active backend with:

```console
cargo run -p norted-server -- unload
```

Unload transitions through `Stopping`, waits for the owned child to exit, uses a force-termination fallback when necessary, and clears the active model. If the model is still `Loading`, unload cancels startup and terminates the loading child before returning to `Stopped`. An unexpected child exit moves the backend to `Failed` and prevents further routing to its stale endpoint. Global `--json` works with load, unload, status, list, configuration, diagnostics, and server-start output.

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
├── load <MODEL_ID>
├── unload
├── models list
├── engines list
├── config show
└── doctor
```

## Public and private endpoints

The public gateway defaults to `127.0.0.1:8742` and exposes:

```text
GET  /health
GET  /v1/models
POST /v1/responses
```

The public API has no authentication in this slice. Keep `server.host` on loopback unless the surrounding network is trusted and protected.

Each loaded llama.cpp backend uses an OS-assigned port on `127.0.0.1`. Norted calls the backend internally at `/health` and `/v1/chat/completions`; it never redirects clients or exposes that endpoint as the model's public URL.

The serving process also binds a different dynamic port on `127.0.0.1` for private `status`, `load`, and `unload` control operations. Its endpoint and a random per-instance bearer token are written to the atomic runtime descriptor in the operating-system state directory. CLI and TUI clients first verify the descriptor's random instance identity through the public `/health` response, then authenticate control requests with the descriptor token. The token is redacted from debug output and is not returned by public routes, logs, or TUI screens. Runtime descriptor permissions are restricted on Unix where the platform permits it.

## Responses text subset

The required request fields are `model` and `input`. The only accepted top-level fields are:

- `model`: a stable discovered model ID;
- `input`: either a string or a non-empty array of text message items;
- `instructions`: an optional developer-instruction string;
- `max_output_tokens`: an optional positive integer;
- `stream`: an optional boolean, defaulting to `false`.

A non-streaming request can use simple string input:

```console
curl http://127.0.0.1:8742/v1/responses \
  -H "Content-Type: application/json" \
  -d '{"model":"<MODEL_ID>","input":"Reply with exactly: Norted works."}'
```

Message input accepts `system`, `developer`, `user`, and `assistant` roles. `type`, when present, must be `message`; `content` may be a string or an array containing only `input_text` parts:

```json
{
  "model": "<MODEL_ID>",
  "instructions": "Answer briefly.",
  "input": [
    {
      "type": "message",
      "role": "user",
      "content": [
        { "type": "input_text", "text": "Count from one to five." }
      ]
    }
  ],
  "max_output_tokens": 128,
  "stream": true
}
```

Non-streaming calls return an OpenAI-style Response object containing an assistant `message` with `output_text`. Responses report the numeric effective llama.cpp `temperature` and `top_p` captured after backend readiness. A llama.cpp `length` finish reason produces `status: "incomplete"` with `incomplete_details.reason: "max_output_tokens"`; normal completion produces `status: "completed"`. `completed_at` is emitted only for completed responses, and token usage is omitted until llama.cpp supplies trustworthy counts. Streaming calls return Norted-generated Responses SSE events, including response/item/content start events, `response.output_text.delta` events, matching done events, and a terminal `response.completed`, `response.incomplete`, or `response.failed` event. Reaching `max_output_tokens` uses `response.incomplete`, not a failure. llama.cpp-native SSE bytes are never passed through.

All unlisted top-level fields are rejected instead of ignored. That includes tools and function calling, hosted web/file search, computer use, code interpreter, remote MCP, image or audio input/output, embeddings, reasoning controls, `previous_response_id`, background mode, persistent storage, and structured-output controls. Non-message input items and non-`input_text` content parts are also rejected. Malformed, unsupported, missing-model, unloaded-model, crashed/unavailable-backend, inference-timeout, and other inference failures use an OpenAI-style `error` envelope with an appropriate HTTP status.

## Provenance and lifecycle records

While loading, Norted records the stable model ID, canonical artifact path, optional pre-existing model content hash, upstream engine ID, reported version/revision when known, canonical binary path and SHA-256, external-binary acquisition method, a source repository only when established, host platform and architecture, explicitly applied normalized settings, conservatively redacted native arguments, configured environment names and value hashes, whether the parent environment was inherited, process ID and supervisor identity, private backend endpoint, and launch timestamp.

Norted does not hash a multi-gigabyte model merely for discovery, does not fabricate an unavailable upstream revision, and does not retain raw configured or inherited environment values. The last runtime provenance remains available in normalized control status after unload for diagnostics, while the active model and engine are cleared.

## Configuration and discovery

If `config.toml` is absent, defaults are used without creating or overwriting it. Relative paths resolve against its directory. Unknown structured keys and unsupported schema versions are rejected; adapter-native maps remain namespaced and open-ended, although the llama.cpp adapter validates the keys it understands.

Configuration, application data, runtime state, cache, and logs use separate operating-system application directories. The TUI enters terminal mode and draws a truthful pending first frame before starting model discovery and control observation in the background. `models list` and `serve` await deterministic discovery; `status` does not initiate a model scan and reports `NotScanned` when appropriate. `engines list` and `config show` also do not initiate a scan. `NO_COLOR` disables TUI colour independently of configuration, while `tui.unicode = false` selects intentional ASCII glyphs.

`GET /v1/models` retains the audited list envelope and the four Model fields `id`, `object`, `created`, and `owned_by`. `created` is the local artifact's last-modified Unix timestamp. The model registry can recognize the existing Q27 artifact kind for listing, but no Q27 adapter or inference path exists in this release.

Building requires Rust 1.88 or newer. `rust-toolchain.toml` pins 1.88.0 for reproducible toolchain behavior while the manifest declares the truthful MSRV.

## Development and validation

Norted Server follows Norted's speed-first approach. New test code is not created by default, and broad test suites, coverage targets, matrices, and integration harnesses are avoided unless justified by risk. Focused tests remain appropriate for difficult lifecycle, security, provenance, and protocol invariants.

Normal validation uses:

```console
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Feature work should also receive proportionate lifecycle and real-inference smoke validation.

## Current limitations

- One loaded model/backend is supported at a time; there is no swap or scheduling policy.
- llama.cpp must be supplied through `binary_path`; PATH discovery and Norted-managed install/update are deferred.
- Only GGUF text generation is loadable. Q27, NInfer, embeddings, vision, audio, tools, and multimodal inference are not implemented.
- The public compatibility surface is limited to model listing and the documented Responses text subset.
- There is no public API authentication, daemon/service installer, model downloader, or web UI.

See [docs/architecture.md](docs/architecture.md) for component boundaries, process ownership, protocol translation, and dependency direction.
