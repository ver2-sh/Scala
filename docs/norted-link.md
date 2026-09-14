# Norted Link

Norted Link federates Norted-Server instances over an existing Wayfinder network.
Both directions are equivalent: each server discovers the other server's models,
profiles and loaded state, can load/unload an owned profile there, and can serve
its hosted profile through the local OpenAI-compatible API. There is no main node.

## Connect two machines

1. Build the Wayfinder version containing peer services v1 on each machine and
   link their distinct Wayfinder identities using its normal invitation workflow.
   Both advertised peer endpoints must be reachable. Norted does not add IP
   configuration, NAT traversal or a second network.
2. Build Norted-Server on each machine. In each local `config.toml`, enable:

   ```toml
   [link]
   enabled = true
   # Optional when Wayfinder uses its normal OS application-data directory:
   # wayfinder_data_dir = "/path/to/this/machines/private/wayfinder"
   ```

   A relative directory resolves against the Norted configuration directory.
   Run Norted as an account authorized to read Wayfinder's private `control.json`.
   Never share identity directories or copy credentials to the other machine.
3. Restart each Norted server after changing configuration. Use each machine's
   normal local runtime and model/profile setup, then load a profile. The TUI's
   Overview, Models and Model Profiles show host labels and owner-reported state.
   In Model Profiles, select the desired host's profile and use Load/Unload or
   `l`/`u`. The host resolves its own runtime and profile settings.
4. Query `/v1/models` on either Norted API and use an advertised model alias in
   Chat Completions, Responses, Completions or Embeddings. Existing local API
   authentication applies before federation routing. No remote API key is needed
   because the private peer channel is authorized through Wayfinder membership.

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

Norted reads the local Wayfinder control descriptor, observes its stable ID and
membership, and registers `norted.link.v1` at an ephemeral authenticated loopback
service. The application registration has a fresh random credential, distinct
from public Norted API keys and Wayfinder's MCP token.

The refresh loop renews registration and queries reachable Wayfinder members for
Norted state. It waits three seconds between cycles, queries up to eight peers
concurrently, limits an individual observation to four seconds and a peer sweep
to ten seconds. A member without a responding Norted service does not appear as
an active Norted peer. A responding incompatible inventory is shown unavailable.
Known peer observations become stale after 15 seconds without successful refresh;
failed observations are marked unavailable immediately. Removed Wayfinder members
are removed from the observation cache. Reconnect discovers current state without
restoring peer files. After an application crash its old Wayfinder registration
may remain until the 60-second lease expires.

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
Norted consumes only its local control/service contract, with no Rust dependency
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
