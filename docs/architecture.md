# Architecture

Norted Server is a set of engine-neutral foundations composed by the `norted-server` binary. A TUI or CLI invocation has its own in-memory `ApplicationCore`; only components inside that process share its `Arc<ApplicationCore>`. Separately launched processes observe a serving API through an explicit runtime-state boundary.

```text
CLI / TUI process                         serve process
┌────────────────────┐              ┌────────────────────┐
│ own ApplicationCore│              │ own ApplicationCore│
└─────────┬──────────┘              └─────────┬──────────┘
          │ reads descriptors                  │ publishes descriptor
          ▼                                    ▼
   state/runtime/servers/<instance-id>.json (atomic, per instance)
          │
          └── GET /health ── verifies the same random instance identity
```

The descriptor includes schema version, instance identity, PID, endpoint, probe address, and start timestamp. PID is informational and is never trusted as proof of liveness. Each server writes a uniquely named file by syncing a temporary file and atomically renaming it, and removes only its own file during graceful shutdown. Observation probes descriptors concurrently, retains IPv4/IPv6 identity checking, and is bounded per probe and per cycle. After an unclean exit, a failed or identity-mismatched health probe makes the descriptor non-running. Cleanup requires startup grace plus either an identity mismatch or an old, definitively unreachable endpoint. Timeout cleanup requires a descriptor older than 24 hours and matching failure evidence from at least three observations spanning an hour; a healthy observation clears that evidence. Before removal, the descriptor is reread and must still match the observed instance. This directory is the narrow bootstrap seam for a future local control/admin channel.

## Crate boundaries

`norted-server` is the composition root. It parses commands, initializes structured logging, constructs services, launches the TUI or API, handles shutdown, and translates failures. Doctor is dispatched before normal configuration loading and directory setup so it can diagnose failures in those steps itself.

`norted-core` owns platform paths, version-1 TOML configuration, resolved model paths, artifact discovery, runtime observation, application events, stable identifiers, and provenance. The registry explicitly moves through `NotScanned`, `Scanning`, `Ready`, `ReadyWithWarnings`, or `Failed`. Recursive discovery runs on Tokio's blocking pool and does not follow symlinks. Relative model roots resolve against the configuration directory. Overlapping roots deduplicate the same canonical artifact. The TUI starts discovery after terminal initialization and receives completion through application events; registry-dependent CLI/server paths await it, while `status` and unrelated commands do not trigger it.

Model IDs have the form `<sanitized-name>-<12-hex-digest>`. The digest is SHA-256 over a versioned identity namespace, artifact format, and normalized canonical path. This is deterministic across Rust releases, keeps same-named files in different directories distinct, and avoids hashing multi-gigabyte contents. The optional model-provenance seam can later supply a stronger logical identity without parsing Norted metadata today.

`norted-engine` owns engine contracts, not implementations. Artifact formats describe artifacts only. Registered adapters declare accepted formats as a cheap capability gate and make the final compatibility decision for each concrete `ModelArtifact`. An adapter can reject an otherwise accepted container because of architecture, model capability, or artifact-specific constraints and supply a human-readable reason. `EngineRegistry` registers, looks up, enumerates, rejects duplicate stable IDs, and uses those concrete decisions when querying compatible adapters. It is empty today.

Acquisition requests explicitly carry stable/latest/exact intent plus installation, build, platform, architecture, and runtime-variant context. Exact acquisition uses a selector that structurally requires a version, source revision, or both. Installation provenance can retain source, acquisition method, exact revision, binary path and SHA-256, build options/toolchain, platform, architecture, runtime variant, and timestamp. Runtime provenance links model identity and optional hash to an exact engine revision, resolved settings, redacted native inputs, process identity, and launch time; unavailable facts remain optional.

Adapters translate normalized requests into `LaunchSpec` and provide engine-specific probe semantics. A separate common `ProcessSupervisor` boundary owns future spawning, stdout/stderr, tracking, and termination. No process supervisor or engine adapter is implemented in this bootstrap.

`norted-api` owns the public HTTP protocol and server lifecycle. It implements only `GET /health` and `GET /v1/models`. Model listing uses the OpenAI-style list envelope and the current Model fields `id`, `object`, `created`, `owned_by`, and optional nullable `shutdown_date`; local models emit a null shutdown date. `created` is the artifact's last-modified Unix timestamp. The server completes initial discovery before binding, so model listing receives a ready registry. This is limited Models-list compatibility, not full OpenAI compatibility. The Responses API remains planned primary inference work and is not present.

`norted-tui` owns terminal lifecycle, UI-local state, commands, responsive shell, screen modules, design tokens, and rendering. Centralized `Theme` and `Glyphs` select color/NO_COLOR and Unicode/ASCII presentation. Rendering is event-driven; a background low-frequency runtime observer publishes only state changes, so terminal input never awaits network health probes and unchanged observations do not redraw. Model scanning and hashing never occur in render paths. The terminal guard tracks and restores every successfully enabled mode on clean exit, event errors, partial initialization, and panics while preserving the previous panic hook.

## Configuration and safety

Configuration defaults to `127.0.0.1:8742`, never a public bind address. Platform-native config, data, state, cache, and log directories are distinct. Existing configuration is read but never overwritten. Schema version `1` is enforced and structured misspellings are rejected, while namespaced engine-native maps remain open-ended. Provenance environment entries retain names and optional value hashes rather than raw secrets.
