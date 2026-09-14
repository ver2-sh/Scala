# Norted Link v1 validation — 2026-09-14

The implementation was audited against upstream Norted-Server `4def0bf` and
Wayfinder `848305b`, then validated from the local modified workspaces. Changes
were not pushed. No new repository test cases were added; two existing API
fixtures were adjusted for the changed state/model contracts.

## Existing checks

Norted-Server passed:

```sh
./validate.sh
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --release -p norted-server
git diff --check
```

`validate.sh` runs formatting, workspace check, Clippy with warnings denied and
workspace tests. Result: 214 passed, zero failed, one existing ignored test.
Wayfinder also passed formatting, workspace check, Clippy with warnings denied,
workspace tests and its release build. Its workspace contains no test cases;
its transport was exercised directly below.

## Direct integration environment

Two actual Wayfinder release daemons and two Norted servers ran in isolated
directories with distinct stable node identities and private listener ports on
one Linux host with an RTX 5090. The same existing 4B Q4_K_M GGUF and existing
llama.cpp binary were referenced by independent owner profiles `profile-a` and
`profile-b`, each with 2048 context. No model or runtime bytes were copied.

Temporary drivers outside either repository used the actual public HTTP API,
authenticated private control API, pinned Noise transport and Ratatui processes.
These were real native-backend generations, not mocked response servers. Existing
production services, their profiles and configuration were left running unchanged.

## Results

148 direct assertions passed across inference/control, failures, generic
transport, TUI, compact rendering and application/authentication checks.

Both A → B and B → A passed:

- Automatic peer discovery; owner model/profile inventory, loaded state, engine
  and runtime identity; remote loaded entries in `/v1/models`.
- Local inference and remote unqualified/fully qualified profile invocation.
- Non-streaming and SSE Chat Completions, Responses and Completions with original
  model aliases, normal terminal events and usage fields.
- Embeddings capability rejection from the actual owner. The selected text
  runtime has no embedding capability; no successful embedding generation is
  claimed by this smoke run.
- Original-client disconnect during streamed and in-flight non-streamed
  generation, with the owner's active request lease returning to zero.
- Remote unload and load through the existing manager; unloaded remote inference
  rejects without JIT loading; refreshed owner state restores usability.
- Duplicate profile aliases return 409, listings use full stable-ID qualification,
  and exact qualified requests execute the selected owner. Ambiguous model
  retrieval also rejects. Deleting an owner profile after discovery returns an
  error without fallback.
- Rejection of invalid hops, source, target, protocol version, mismatched model
  identity, unknown runtime operations, arbitrary API paths, bad local credentials
  and oversized protocol frames.
- Real TUI Models/Profiles/Overview display both hosts. TUI load/unload reaches
  the selected owner and preserves the local backend. Remote edit shortcuts do
  not mutate local profile files. Runtime views contain only the local runtime
  state. Owner reports remain readable by scrolling at 80×24.

Additional lifecycle/security checks passed:

- Stopping B's Wayfinder during a real SSE generation reports
  `link_stream_interrupted`; A and B keep serving locally. B becomes unavailable,
  its remote alias leaves the usable listing, and restarting Wayfinder restores
  discovery and inference in both directions.
- A third actual Wayfinder member with no Norted service stays out of the active
  Norted peer list. Starting Norted there with required API authentication allows
  authenticated remote inference and rejects unauthenticated inference/listing.
  Public keys cannot authorize private Link control; private control credentials
  cannot authorize the public API. Spoofed internal Link headers reject, and
  private Link control is absent from the public listener.
- Stopping only a peer's Norted process marks that application unavailable while
  Wayfinder stays up. Application restart, normal local load and state refresh
  restore remote inference naturally.
- An intentionally incompatible owner profile reaches normal q27 validation and
  reports that q27 requires a Q27 artifact for the GGUF fixture. The failure is
  advertised; the owner's original loaded profile keeps running. The temporary
  invalid profile is removed afterward.
- Profile stores retain their own original profiles; no peer runtime installation
  or model files appear in the isolated local data directories.

Wayfinder's separate generic service checks passed in both directions: private
registration, takeover and non-loopback rejection, 32 MiB echo integrity,
slow-reader backpressure, disconnect propagation, missing-service errors,
unregister, and rejection of the MCP bearer at the service-open listener.

The isolated validation daemons, TUIs and native backends were stopped afterward.
Their private control descriptors were removed and listener closure was checked;
the original production processes remained running. Scratch validation records
remain outside both repositories at `/srv/norted/scratch/norted-link-audit`.

## Scope

The second physical machine has not been connected yet. This is a completed
infrastructure implementation with real multi-process validation on one host;
physical LAN/firewall and Windows/macOS behavior remain unvalidated. Native smoke
generation used llama.cpp; the existing q27, NInfer, DFlash2 and vision contracts
were covered by the existing workspace checks, not new hardware inference runs.

Use [the connection instructions](norted-link.md#connect-two-machines) on each
machine. Both require this Wayfinder peer-service version and Link enabled in the
normal typed Norted configuration. No production service was activated for this
validation.
