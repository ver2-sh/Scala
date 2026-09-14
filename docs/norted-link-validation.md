# Norted Link v1 validation — 2026-09-14

## Peer-service privilege boundary revalidation — 2026-09-14

This pass starts from Wayfinder `1cb5cbc5488eb7404f07074bf28813a34eb7985a`
and Norted-Server `499de722494ab361c5211cc8998845b14c562a08`. Changes remain
local and unpushed. Earlier results below describe the previous implementation;
the application credential instructions in the current peer-service/Link docs
supersede that implementation's full-control discovery contract.

Wayfinder now publishes an explicit, separate `PeerServiceDescriptor` for each
`daemon --peer-service SERVICE=/absolute/path` option. The random capability is
independent of administration and MCP credentials. `/peer-service` decodes only
sanitized status, registration/renewal and unregistration. Both this API and the
local stream listener enforce the configured service name. The administrator's
`/control` authority is retained; its descriptor no longer advertises a transport
listener. Norted uses only `link.wayfinder_peer_service`, with no old-path fallback.

### Existing workspace checks

Wayfinder passed `cargo fmt --all -- --check`, `cargo check --workspace`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
(no repository test cases), `cargo build --release`, and `git diff --check`.
Norted passed `./validate.sh` (214 tests passed, one existing ignored),
`cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`cargo build -p norted-server`, `cargo build --release -p norted-server`, and
`git diff --check`. No repository tests were added.

### Isolated boundary proof

External driver `security.py` passed 58 assertions against two fresh Wayfinder
instances using the final release binary:

- Observation contains only `nodes` and `conflict`; every node contains exactly
  `id`, `name`, `local`, and `reachable`.
- Service capability differs from both admin and MCP credentials.
- Admin status/details/create/invite/join/remove reject the service bearer with
  HTTP 401. Administrative operations also fail the service API decoder (422).
- MCP rejects the service bearer (401). Private identity/config/state paths and
  `/control` do not exist on the service endpoint (404).
- Register/unregister/open of an unrelated service name fail scope checks.
  The admin bearer also fails authentication at the application stream listener.
- Registration, renewal, unregistration, unavailable-after-unregister and actual
  bidirectional Noise echo streams pass.
- A separate Unix UID (65534), granted only the 0600 capability file, successfully
  observes members while filesystem reads of identity.json, config.json,
  state.json and control.json all fail with PermissionError.

### Norted regression

The prior two-node smoke/failure drivers were adapted outside the repositories
to use the new capability paths. Fresh Wayfinder identities and independent
Norted profiles reference an existing GGUF and llama.cpp binary; no model/runtime
bytes are copied. An old setup-driver assumption about Norted's runtime descriptor
location failed initially; the driver was corrected to use `runtime/servers`.
This was a harness failure, not an application failure.

Both Norted processes then ran in private mount namespaces masking their
Wayfinder private directories with empty directories. `/proc/PID/root` checks
confirmed that neither process could see Wayfinder identity/config/state/control
files. The separate service descriptors remained visible. With that restriction:

- `smoke.py`: 42 assertions passed, both A→B and B→A. Discovery, remote inventories,
  local and qualified remote inference, nonstreaming/streaming Chat Completions,
  Responses and Completions, owner embedding capability rejection, streaming and
  nonstreaming cancellation, owner load/unload, no JIT load and state refresh.
- `failures.py`: 44 assertions passed. Source/target/hop/version/size/operation
  rejection, exact owner routing, deterministic duplicate aliases, no fallback
  after owner profile removal, explicit interrupted-stream error, stale peer
  handling, local inference during Wayfinder loss, reconnect and restored remote
  inference in both directions. Independent profile stores remained intact and
  neither model nor runtime installations appeared in the peer data directories.

- `tui.py` and `tui-actions.py`: real Ratatui captures show both nodes in
  Models/Overview and the selected remote owner in Profiles; 16 action assertions
  passed for owner load/unload, unchanged local backends/profile files, disabled
  remote editing and local-only Runtime views.
- `cleanup.py`: 12 capture/lifecycle assertions passed. Isolated TUIs, Norted
  servers, native backends and Wayfinder daemons were stopped; Norted listeners
  closed and administration/application capability descriptors were removed.
  Production processes were not targeted.

Reproduction commands (drivers are intentionally outside the repositories):
`python3 security.py`, `python3 setup.py` with its runtime descriptor lookup
corrected by `python3 load.py`, `python3 restricted.py`, `python3 load.py`,
`python3 smoke.py`, `python3 failures.py`, `python3 tui.py`,
`python3 tui-actions.py`, and `python3 cleanup.py` from the scratch directory.

Scripts, logs and assertion records are in
`/srv/norted/scratch/norted-link-boundary`, outside both repositories. This is
real multi-process Linux/llama.cpp validation on one host; no second physical
machine or Windows/macOS deployment was tested. Existing q27/NInfer/DFlash2/vision
contracts passed workspace checks; no new hardware inference runs for those
engines are claimed. No federation semantics or runtime/model implementations
were changed. Capability files are ephemeral: regrant application file access
after restart and explicitly remove a stale descriptor after an unclean stop.


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
