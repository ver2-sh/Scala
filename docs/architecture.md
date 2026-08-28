# Architecture

Norted Server is an engine-agnostic local inference control plane composed by the `norted-server` binary. The first engine integration is llama.cpp, but llama.cpp-specific flags, probing, health checks, and backend JSON remain behind the common engine boundary.

There are three distinct network surfaces:

```text
                                              serve process
                                      ┌───────────────────────────┐
client ── configured public address ─▶│ public Axum gateway       │
                                      │ /health                   │
                                      │ /v1/models                │
                                      │ /v1/responses             │
                                      └─────────────┬─────────────┘
                                                    │ engine-neutral inference
                                                    ▼
                                      ┌───────────────────────────┐
                                      │ RuntimeManager            │
                                      │ one active backend        │
                                      └───────┬───────────▲───────┘
                                              │           │
                                  launch/infer│           │status/load/unload
                                              ▼           │
                                      ┌───────────────┐   │
                                      │ llama.cpp     │   │
                                      │ adapter       │   │
                                      └───────┬───────┘   │
                                              │           │
                                  127.0.0.1:<dynamic>      │
                                              ▼           │
                                      ┌───────────────┐   │
                                      │ llama-server  │   │
                                      │ owned child   │   │
                                      └───────────────┘   │
                                                          │
CLI / TUI ── descriptor + Bearer token ── 127.0.0.1:<dynamic>
                                      private control listener
```

The configured public listener is the stable client-facing endpoint. It defaults to `127.0.0.1:8742`, but configuration can change it. The llama.cpp backend and control listener are always bound separately on IPv4 loopback with OS-assigned ports. Clients are never redirected to the backend.

## Cross-process runtime and control

A TUI or CLI invocation has its own in-memory `ApplicationCore`; it does not share an `Arc<ApplicationCore>` with a separately launched `serve` process. Cross-process observation and control therefore use the state directory:

```text
state/runtime/servers/<instance-id>.json
```

The schema-version-2 descriptor contains the random instance identity, PID, public endpoint and probe address, private control endpoint and loopback address, random control token, and start timestamp. The serving process writes the descriptor by syncing a uniquely named temporary file and atomically renaming it. On Unix, the runtime directory is set to mode `0700` and the descriptor to `0600`; on other platforms the current code leaves the existing application-state directory ACLs unchanged. Debug formatting redacts the token.

An observer does not trust the descriptor's PID as proof of liveness. It probes public `GET /health` and accepts a descriptor only when the response contains the same random instance identity. Probes run concurrently with per-request and whole-cycle time limits. Identity mismatches and repeated aged failures are cleaned up conservatively; startup-grace descriptors and one-off connection failures or timeouts are retained. A healthy observation clears accumulated failure evidence, and removal rereads the descriptor to ensure that it still matches the observed instance.

After descriptor validation, `ControlClient` reads the private endpoint and token and sends `Authorization: Bearer <token>` to:

```text
GET  /control/v1/status
POST /control/v1/load
POST /control/v1/unload
```

The control listener binds only to `127.0.0.1`, uses a constant-time token comparison, and is not part of the public router. The token is not returned by `/health`, `/v1/models`, `/v1/responses`, normalized control status, CLI/TUI output, or logs. Control status carries registered engine probes, installed/running counts, backend lifecycle, active model and engine, process/private endpoint diagnostics, bounded recent runtime notices, and provenance without exposing the credential.

## Crate boundaries

`norted-server` is the composition root. It parses commands, initializes structured logging, loads the core, registers `LlamaCppAdapter` in `EngineRegistry`, constructs `RuntimeManager` with `TokioProcessSupervisor`, launches the TUI or API, handles shutdown, and translates failures. A load or unload CLI command discovers the already-running control instance; it does not create a second hidden serving process. Doctor remains dispatchable before normal configuration and directory setup.

`norted-core` owns platform paths, version-1 TOML configuration, resolved model paths, artifact discovery, stable identifiers, application events/state, runtime descriptor publication and observation, and provenance structures. The registry moves through `NotScanned`, `Scanning`, `Ready`, `ReadyWithWarnings`, or `Failed`. Recursive discovery runs on Tokio's blocking pool, does not follow symlinks, resolves relative roots against the configuration directory, and deduplicates overlapping canonical artifacts.

Model IDs have the form `<sanitized-name>-<12-hex-digest>`. The digest is SHA-256 over a versioned identity namespace, artifact format, and normalized canonical path. It is deterministic across Rust releases, keeps same-named files in different directories distinct, and avoids hashing multi-gigabyte contents. The registry still recognizes the existing Q27 artifact kind for discovery, but no Q27 or NInfer engine is implemented and the llama.cpp adapter rejects non-GGUF artifacts.

`norted-engine` owns engine-neutral contracts and their first concrete runtime services:

- `EngineRegistry` registers stable adapter identities and applies concrete artifact compatibility decisions.
- `RuntimeManager` composes the engine registry, process supervisor, discovered model registry, active state, inference routing, recent notices, and runtime provenance.
- `TokioProcessSupervisor` owns generic child spawning, PID/supervisor identity, continuous stdout/stderr draining, structured tracing, bounded stderr tails, exit observation, termination, and shutdown cleanup.
- `ControlClient` owns descriptor-based discovery and authenticated private control requests.
- the minimal inference representation contains text messages, explicit maximum output tokens, streaming intent, text deltas/output, finish reason, and optional token usage.

The engine acquisition contracts retain stable/latest/exact intent, but individual adapters must report what they actually implement.

`norted-engine-llama-cpp` contains all current llama.cpp-specific behavior. It declares upstream identity `llama.cpp` and `https://github.com/ggml-org/llama.cpp`, accepts GGUF at the current coarse compatibility boundary, and declares only text generation through the backend's Chat Completions-compatible API. It does not claim vision, embeddings, tools, structured output, or native Responses support.

`norted-api` owns the public protocol and both Axum listener lifecycles. The public router implements `GET /health`, `GET /v1/models`, and the narrow `POST /v1/responses` subset. The private router implements only authenticated status/load/unload. Public Responses parsing and serialization remain independent from llama.cpp-native JSON.

`norted-tui` owns terminal lifecycle, UI-local state, commands, responsive rendering, and interaction. It enters terminal mode and draws an initial pending frame before starting discovery or control observation, so a slow filesystem or stale descriptor cannot delay visible startup. A low-frequency observer then obtains normalized control status from the serving process, so Overview, Models, Engines, and Server render real cross-process state rather than process-local placeholders. Models Enter and `/load` load the selected artifact; `u` and `/unload` unload the active backend. Single-click continues to select rather than start a potentially large load.

## llama.cpp configuration and probing

The minimal accepted configuration is:

```toml
[engine."llama.cpp"]
enabled = true

[engine."llama.cpp".settings]
binary_path = "/path/to/llama-server"
```

A relative `binary_path` resolves against the directory containing `config.toml`. The adapter supports only the `binary_path` settings key. Its open engine-native escape hatches are `[engine."llama.cpp".native].arguments` (an array of strings) and `[engine."llama.cpp".env]` (string values).

The probe:

1. requires the adapter to be enabled and the configured path to resolve to a file;
2. canonicalizes the path;
3. runs `--version` and `--help` with bounded execution;
4. requires help output to advertise `--model`, `--alias`, `--host`, and `--port`;
5. parses version/revision only when reported; and
6. calculates the binary SHA-256.

A successful probe creates an `EngineInstallation` whose acquisition method is `ExternalBinary`, source repository is absent because a configured executable does not prove its source, build metadata is absent, platform/architecture describe the host, and unavailable revision/runtime-variant facts remain `None`. The adapter's canonical upstream identity remains llama.cpp. The adapter does not cache probes: every `probe()` call repeats path validation, version/help inspection, and binary hashing. The runtime manager stores the most recently observed result for status, while engine selection and launch obtain fresh probe evidence.

Managed installation and updating intentionally return unsupported errors. There is no PATH discovery, release scraping, source build, downloader, or updater in this slice.

## Process launch and lifecycle

Norted preserves upstream defaults. The adapter's generated command line contains only:

```text
llama-server --model <canonical-gguf> --alias <stable-model-id> --host 127.0.0.1 --port <dynamic> [configured native arguments...]
```

It does not synthesize context size, GPU layers, batch size, threads, cache types, flash attention, tensor split, speculative decoding, or a chat template. Explicit native arguments are appended in order, but Norted rejects options that replace the primary model source, stable alias, private host/port, API prefix/authentication, TLS, embedding/reranking mode, or multi-model router semantics. The same critical llama.cpp variables are rejected in configured environment entries. The launch spec still inherits the parent environment, but its generic `environment_remove` list strips those variables before allowed configured entries are applied. Ordinary tuning arguments and environment variables remain available. A future engine can use the same generic removal contract or disable inheritance entirely.

The runtime has exactly one backend state machine:

```text
Stopped ── load ──▶ Loading ── health ready ──▶ Running
Loading ── startup failure ─────────────────────▶ Failed
Loading ── unload/cancel ──▶ Stopping ── cleanup ──▶ Stopped
Running ── unload ─────────▶ Stopping ── cleanup ──▶ Stopped
Running ── unexpected exit ─────────────────────▶ Failed
Failed  ── unload ──────────────────────────────▶ Stopped
```

Loading resolves the stable model ID from the completed discovery registry, selects one compatible healthy installed adapter, obtains an OS-assigned loopback address, asks the adapter for a `LaunchSpec`, and delegates spawning to the common supervisor. The manager then polls llama.cpp `GET /health` every 250 ms for up to five minutes while also observing process exit. After health returns `status: "ok"`, the adapter reads and validates `default_generation_settings.params.temperature` and `top_p` from `GET /props`. The manager stores those values with the active route and reports `Running` only after both checks succeed.

The supervisor starts the process with piped stdout/stderr and null stdin. Dedicated drain tasks prevent pipe deadlock and forward lines to structured tracing with engine ID, model ID, process ID, and stream name. Unexpected exit detail includes a bounded stderr tail. On unload, Unix sends SIGTERM before a timeout/kill fallback; Windows uses the available child-process termination primitive. Serving-process shutdown unloads the active backend and then asks the supervisor to terminate any remaining tracked children.

Loading another model while one is active returns an explicit conflict; there is no implicit replacement. Unload can cancel an in-progress load, terminating a child that has already spawned before clearing state. A later crash clears the routable active backend, retains failure detail and provenance, and causes inference routing to fail instead of targeting a stale endpoint.

## Runtime provenance

The manager constructs provenance from facts observed during the actual load:

- stable model ID, canonical artifact path, and an optional model content hash if one already exists;
- engine ID plus reported version/revision when known;
- canonical binary path, binary SHA-256, optional proven source repository, and `ExternalBinary` acquisition method;
- host platform/architecture and optional runtime variant;
- normalized settings actually emitted (none are currently synthesized for llama.cpp);
- configured native arguments, retaining bare dash-prefixed arguments but redacting every non-option argument and every value in a dash-prefixed `name=value` form;
- explicitly configured environment variable names and SHA-256 value hashes, never raw values;
- an explicit `inherits_parent_environment` boolean (true for llama.cpp); inherited environment entries are not copied into provenance;
- process ID and supervisor identity;
- private backend endpoint and launch timestamp.

Missing upstream facts remain optional. Model discovery does not hash an entire GGUF merely to populate provenance. Unload clears active model/engine state but intentionally leaves the most recent provenance available for diagnostics.

## Public Responses translation

`POST /v1/responses` accepts only `model`, `input`, `stream`, `instructions`, and `max_output_tokens`. `input` is either a string or a non-empty array of message items. Supported message roles are system, developer, user, and assistant; content is a string or an array containing only `input_text` parts.

The API translates this request into the engine-neutral `InferenceRequest`. The llama.cpp adapter then maps messages to its private `POST /v1/chat/completions` call and maps an explicit `max_output_tokens` to `max_completion_tokens`. For streaming it asks the backend to include usage. The runtime returns the active backend's engine-neutral effective generation settings atomically with each inference route. The public API crate never builds llama.cpp flags, reads `/props`, or decodes llama.cpp-specific completion JSON.

For non-streaming inference, the adapter returns assistant text, optional usage, and a normalized finish reason. The API constructs a local Response object and assistant message/`output_text` item with locally generated response and message IDs, plus numeric `temperature` and `top_p` from the active route. llama.cpp `finish_reason: "length"` maps to an incomplete Response with `incomplete_details.reason: "max_output_tokens"`; other current finish reasons map to completed. `completed_at` is present only for completed snapshots, unavailable usage is omitted, and unavailable output-text logprobs are not fabricated.

For streaming inference, the adapter parses llama.cpp SSE into engine-neutral text deltas and a completion event. The public API emits a Responses event sequence with monotonically increasing `sequence_number`:

```text
response.created
response.in_progress
response.output_item.added
response.content_part.added
response.output_text.delta (zero or more)
response.output_text.done
response.content_part.done
response.output_item.done
response.completed | response.incomplete
```

The adapter carries llama.cpp's final finish reason through the engine-neutral completion event. Each streaming snapshot uses the generation settings captured with its route. A normal stream terminates with `response.completed`; a `length` finish caused by the output-token limit terminates with `response.incomplete` and `incomplete_details.reason: "max_output_tokens"`. A backend failure after streaming starts instead terminates with `response.failed`. Initial and non-completed terminal snapshots omit `completed_at`; usage is emitted only after it is supplied. llama.cpp's native chunks and `[DONE]` marker are consumed internally and never forwarded verbatim.

Every unrecognized top-level request field is rejected with an OpenAI-style error envelope. Non-message items, non-`input_text` parts, tools/function calling, hosted tools, image/audio, remote MCP, reasoning controls, prior-response state, background mode, storage, and structured-output controls are therefore rejected rather than silently ignored. Public error mapping distinguishes malformed/unsupported requests, missing models, unloaded models, crashed or unavailable backends, inference timeouts, and other backend inference failures without disclosing the control token or raw environment values.

`GET /v1/models` remains unchanged: it emits the OpenAI-style list envelope and only `id`, `object`, `created`, and `owned_by` for each discovered artifact. The route does not imply that every discovered artifact has a compatible installed engine.

## Configuration and safety invariants

Configuration schema version `1` is enforced. Structured misspellings are rejected, while engine-native maps remain namespaced and extensible. Existing configuration is read but never overwritten. Configuration, data, state, cache, and logs occupy separate platform-native directories.

The scriptable `status` path snapshots in-memory registry state and cross-process control facts without initiating model discovery. A fresh invocation can therefore report `NotScanned` immediately. Commands that require the artifact registry, including `models list` and `serve`, still await deterministic discovery.

The current hard boundaries are:

- private control and inference backend listeners are loopback-only;
- the descriptor token stays out of all public and normalized status surfaces;
- only the supervisor owns generic child-process mechanics;
- only adapters own engine-native flags, probes, health, and inference JSON;
- only the public API layer owns Responses request/response/SSE shapes;
- at most one backend is active;
- only GGUF text generation is implemented;
- managed llama.cpp install/update, Q27, and NInfer remain deferred.
