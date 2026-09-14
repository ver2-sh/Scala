# Norted Link

Norted Link federates Norted-Server instances over an existing Wayfinder network.
Both directions are equivalent: each server discovers the other server's models,
profiles and loaded state, can load/unload an owned profile there, and can serve
its hosted profile through the local OpenAI-compatible API. There is no main node.

The product boundary is:

```text
TCP/network
  ↓
Wayfinder: identity / membership / encrypted application service transport
  ↓
Norted Link: models / GPUs / profiles / inference / remote load state
  ↓
Local Norted runtime: NInfer / llama.cpp / q27 / etc.
```

Wayfinder discovers generic nodes. Norted independently opens `norted.link.v1`
on reachable nodes and accepts compatible owner reports. Nodes without that
service are normal and remain absent from Norted's peer list. Wayfinder maintains
no application classification or central application registry.

Owner hardware reports include OS/architecture, detected accelerator names and
reported total VRAM (unknown stays unknown). Total VRAM helps identify hosts; it
is not live free-memory telemetry or a scheduling decision. Norted also reports
model metadata/provenance, profiles and roles, loaded lifecycle, runtime
identity/version/variant, effective context and parallelism, activity and existing
benchmark summaries. These observations never configure the receiving host.

## Connect two machines

1. Build the Wayfinder version containing peer services v1 on each machine and
   link their distinct Wayfinder identities using its normal invitation workflow.
   Both advertised peer endpoints must be reachable. Norted does not add IP
   configuration, NAT traversal or a second network.
2. Set only this Norted configuration (or run `norted-server link enable`):

   ```toml
   [link]
   enabled = true
   ```

3. Start Norted. Norted automatically discovers the generic local
   application socket and registers `norted.link.v1`. No address, credential,
   capability file or application setup in Wayfinder is needed.

The **Norted Link** screen shows connection state, full local node ID and peer
observations. Press `e` to enable or `x` to disable; `norted-server link status`
reports saved configuration and observed state. Saved enable/disable changes
apply when Norted starts. Wayfinder does not need a restart.

Load each owner's normal local profile, or select a remote profile in the TUI
and use `l`/`u`. Overview, Models and Model Profiles show host labels and owner
state; Runtime screens stay local. Query `/v1/models` and use an advertised alias
in Chat Completions, Responses, Completions or supported Embeddings. Existing
local API authentication applies before routing.

The setting defaults to disabled. With Link disabled, local serving behaves as
before. With Link enabled but Wayfinder unavailable, local serving remains
available; the Link observation reports the integration error and retries
discovery. Norted never starts or administers Wayfinder automatically.

## Authority and boundaries

The local Model Registry, Model Profiles store, runtime manager and benchmark
store remain authoritative only for this machine. Norted queries owner reports
and keeps peer observations in memory. It does not mirror peer profiles into
`model-profiles.json` or peer state into any authoritative local file.

The inventory report includes artifact identity/format, technical metadata,
acquisition and package provenance, profile identity/name/role/engine, installed
state, load status/progress/failure, execution activity, effective context where
reported, and loaded runtime identity/version/variant. It includes a bounded
projection of the latest existing benchmark summary for each profile. Runtime
configuration, runtime paths, backend endpoints and process credentials are not
inventory fields. An owner reports the facts; another node does not infer engine
capabilities or promote metadata into serving authority.

Runtime installation, binaries, selection, caches, CUDA/device settings and host
paths remain strictly local. Runtime screens and Settings still use only local
runtime managers and stores. A peer can request profile load/unload, but cannot
install/update a runtime, alter its configuration, rebind/edit a profile, browse
files, run a benchmark or execute a shell through the Norted Link protocol.

Model files are never transferred. A model installed only on B is visible and
invokable from A while B hosts it. Artifact provenance has no special serving
semantics: equivalent artifacts pass through the same adapters and validation.

## Discovery and freshness

Norted connects to the generic `/run/wayfinder/app.sock` endpoint. Wayfinder and
Norted may run under separate Linux service accounts. The service installers
provision the generic `wayfinder-apps` group; Norted's unit receives it through
`SupplementaryGroups`. Install/update the Wayfinder service to provision its
protected runtime directory, and install/update the Norted service to provision
application access. The installers retain their respective repository owners as
service accounts; provision the checkouts for distinct accounts when isolation
is required. No service name is configured inside Wayfinder.

For manual application accounts, an administrator grants generic local Wayfinder
application access with `sudo usermod -aG wayfinder-apps APP_USER`, then starts a
new login/session. The group must exist first. This is one-time OS provisioning,
not a Link setting. Source-development instances can explicitly share
`XDG_RUNTIME_DIR` with a provisioned `wayfinder` subdirectory; an ordinary
login runtime directory alone does not override the machine endpoint. See Wayfinder's generic local application documentation for
protected directory provisioning. Norted has no socket, group, UID or credential
configuration field.

Norted reads no Wayfinder private state or control descriptor. Linux socket
permissions authorize the application; Norted verifies endpoint permissions and
checks the daemon peer UID against the protected directory owner. Access to the
socket grants no access to Wayfinder identity/config/state/control files, MCP or
administration. Dynamic transport is unsupported on Windows/macOS.

Norted keeps a registration session open and binds an ephemeral authenticated
loopback listener for incoming streams. Its randomly generated registration
credential has no Wayfinder MCP or administration authority. Registrations are
owned by the session, disappear on disconnect or process exit, and are never
persisted. Norted reconnects and re-registers after Wayfinder starts or restarts.
No operator action is required; local serving remains available during loss.

The refresh loop waits three seconds between cycles, queries up to eight peers
concurrently, limits individual observations to four seconds and a sweep to ten
seconds. A node without a responding Norted service never becomes a Norted peer.
Known peer observations become stale after 15 seconds; failed observations are
marked unavailable immediately. Removed members leave the observation cache.
Reconnection discovers current state without restoring peer files or replaying
inference/control operations.

The TUI and API read cached remote observations, so a dead peer does not block
their local state reads. API dispatch rechecks the authoritative owner; cached
loaded state is never permission to manufacture a local backend or retry elsewhere.

## Model aliases and routing

Existing local `/v1/models` behavior is preserved: it lists local Model Profiles,
including unloaded profiles that local JIT serving can load. Link adds reachable,
installed, running peer profiles. `owned_by` identifies the full Wayfinder node
ID when identity is available. Unqualified profile IDs remain usable when unique.

The explicit qualified form is:

```text
<profile-id>@<full-64-character-wayfinder-node-id>
```

`@` cannot occur in a local Model Profile ID. Full stable IDs avoid display-name
and short-prefix collisions. Qualified aliases work even when a profile is unique.
If multiple known owners advertise the same profile ID, `/v1/models` qualifies
each listed entry and an unqualified request returns HTTP 409 `ambiguous_model`
with the exact choices. Unloaded and stale known owner profiles also reserve their
names during observation, preventing a disconnect from silently retargeting an
unqualified request to another host. Unreachable entries are excluded from the
usable model listing. Ownership is always shown in TUI model/profile views.

The entry gateway authenticates the client and resolves the alias once. It sends
the original model-addressed JSON and selected session/role/correlation headers
to the exact owner. The owner executes the same existing API parsers, capability
checks, settings resolution and adapter inference paths. The requested alias is
preserved in output. Tools, reasoning, media, structured output, usage and terminal
events have exactly the capabilities and semantics of that owner runtime.

Forwarded inference must acquire an already loaded, non-retiring backend with a
matching profile configuration. It never invokes JIT load or another federation
lookup. Every internal request requires version 1, matching source/destination
identities and hop count 1. The source identity must match Wayfinder's authenticated
preface, and inference aliases must match the explicit hosted profile. Invalid
routes, unknown operations, arbitrary API paths and public `x-norted-link-*`
headers are rejected. Forwarded session IDs are namespaced by the authenticated
source; unrelated local clients cannot accidentally share those leases.

## Protocol and limits

Wayfinder owns connectivity, peer authentication, membership and byte transport.
Norted consumes only its generic local application contract, with no Rust dependency
on Wayfinder internals. See Wayfinder's `docs/peer-services.md`.

After the authenticated Wayfinder service preface and ready reply, one Norted
request is a four-byte big-endian byte length plus UTF-8 JSON:

```json
{
  "version": 1,
  "source": "<initiating Wayfinder ID>",
  "target": "<executing Wayfinder ID>",
  "hops": 1,
  "operation": {"op": "state"}
}
```

Other operations are `{"op":"control","profile_id":"...","action":"load"}`
(or `unload`) and `{"op":"inference","profile_id":"...","path":"/v1/responses",
"body":{...},"headers":{...}}`. Only the four supported POST inference paths are
admitted. JSON contracts reject unknown fields. The encoded envelope limit is
32 MiB + 32 KiB, while the inference JSON itself remains limited to 32 MiB.

The owner returns a framed JSON head with `version`, HTTP `status`, and allowlisted
response `headers`. Then it sends four-byte lengths plus response byte chunks of
at most 32 KiB. A zero-length chunk is explicit successful transport completion;
the application HTTP status/body still determines operation success. Setup/head
JSON is limited to 16 KiB and inventory to 1 MiB, with at most 4096 models and
4096 profiles per report. Membership bounds come from Wayfinder (128 nodes).
Norted admits at most 16 concurrent Link operations per node, including discovery;
Wayfinder independently bounds its own service slots and encrypted records.

There is one operation per stream. Destination prefaces have a five-second deadline,
request reads a 20-second deadline, outbound service opening six seconds, and
outbound envelope writes 15 seconds. A destination operation has a one-hour total
deadline. Peer response waits are bounded to one hour. Private remote-control calls
have a 55-second deadline. No payloads, tokens, keys or peer caches are persisted.

The local TUI uses authenticated `GET /control/v1/link` for the observational
snapshot and `POST /control/v1/link` with
`{"node_id":"...","profile_id":"...","action":"load"}` or `unload`.
These routes are on Norted's private control listener, never its public listener.

## Failure and cancellation semantics

No inference or control request is retried or failed over. Unknown, unloaded,
stale and unreachable targets fail explicitly. The owner re-reads its own profiles
and applies its normal missing-model, runtime compatibility and load checks.
Load returns owner admission/progress; the existing server-owned load task may
continue after the control connection closes. Observe the owner before retrying
an uncertain operation. Background load failures appear in subsequent owner state.

Responses stream incrementally with bounded buffers and TCP backpressure. Closing
the original API request drops its peer stream, closes the destination request,
and releases the ordinary inference lease. This also cancels pending non-streaming
inference. Cancellation does not undo already admitted management operations.
Disconnecting Wayfinder cancels streams without stopping local Norted serving.

Failure before a response returns an actionable gateway error. Peer loss during
SSE produces an `error` event with `link_stream_interrupted`, without fabricating a
normal completion or final usage. Loss during a non-streaming response body fails
the HTTP body. Reconnecting refreshes observations; it never resumes a generation.
