# Norted Link integration validation — 2026-09-14

Inspected and fetched both clean repositories before editing: Norted-Server
`763b315` and Wayfinder `ed6da2d` matched their upstream master branches. No changes
were pushed. No repository tests were added.

## Implemented boundary and setup

Wayfinder persists generic local application-service records and delivers only
service-scoped capabilities. Its CLI configures arbitrary services; its TUI shows
configured and active access. Both daemon startup paths load the same set. Group
read access is reapplied during atomic publication and credentials rotate at
restart. Service configuration changes apply on the next daemon start.

Norted's Link screen and `link enable --capability PATH`, `link disable` and
`link status` commands replace manual configuration as the normal setup flow.
The screen distinguishes saved configuration from running state. Norted restart
applies changes. Explicit capability selection avoids private-directory probing.
An enabled Link with no selected capability reports an actionable error while
local serving remains available. Group-readable descriptors are accepted only
without group write or world access.

The service/inference wire protocol is unchanged. Norted owns peer classification,
owner hardware/model/profile/runtime reports, remote load/unload and inference.
Only compatible responding services enter its peer list. Hardware summary now
includes detected total VRAM, with unknown preserved. Model/runtime stores,
provenance behavior, engines and their local execution logic were not changed.

## Workspace checks

Both `./validate.sh` scripts passed formatting, workspace check, Clippy with
warnings denied and workspace tests. Norted: 214 passed, zero failed, one existing
ignored. Wayfinder: no repository test cases. Norted all-feature/all-target Clippy
also passed. Norted debug executable and Wayfinder release executable built.
`git diff --check` and Wayfinder service-script shell syntax checks passed.

## Real isolated integration

External drivers and captures live at
`/srv/norted/scratch/norted-link-iteration`, outside both repositories. Two fresh
Wayfinder identities and two independently configured Norted processes used the
actual encrypted transport and an existing llama.cpp binary/GGUF on one Linux
host. Separate profiles referenced the existing files; no model/runtime bytes
were copied. Production services were not targeted.

Both Norted processes ran in private mount namespaces masking Wayfinder's
identity/config/state/control directory. A separate Unix UID also accessed a
service capability through its configured group while private files rejected
reads. The application API exposes only sanitized generic nodes, not private
identity or administration data.

- `security.py`: 60 assertions, including generic unrelated service registration,
  bidirectional encrypted echo, separate credentials, admin/MCP rejection,
  cross-service scope rejection and managed group access by a separate UID.
- `lifecycle.py`: 18 assertions, including persistent arbitrary services, actual
  cross-service credential rejection, ordinary restart rotation, retained group
  permissions, crash recovery, TUI-owned/attached daemon behavior, removal on
  restart and a third generic member absent from both Norted peer lists.
- `smoke.py`: 42 assertions in both directions: discovery and owner inventory,
  qualified/unqualified inference, nonstreaming and SSE Chat Completions,
  Responses and Completions, owner embedding capability rejection, streamed and
  nonstreamed cancellation, remote load/unload, no remote JIT and refresh.
- `failures.py`: 44 assertions: exact target/hop/source/version validation,
  deterministic duplicate aliases, no owner-removal fallback, interrupted stream
  errors, stale peers, local inference during Wayfinder loss, restored federation
  after reconnect and intact independent model/profile/runtime stores.
- `tui.py` captures plus `tui-actions.py`: 16 action assertions confirming
  federated Overview/Models/Profiles, remote load/unload and local-only runtime
  management without local profile mutation.
- `link-ui.py`: 14 assertions covering Link status and full IDs on both machines,
  explicit capability input, enable/disable persistence, configured-versus-running
  status, preservation of other configuration tables, 80×24 input visibility and
  local serving with missing capability configuration.
- `cleanup.py`: 12 assertions for federated captures and clean isolated process,
  descriptor and listener shutdown.

The final daemon executable was also restarted on both nodes before repeating
security and inference checks. An immediate smoke attempt reached a cached
pre-restart peer observation before registration recovered; repeating after
normal discovery renewal passed. Operations are not retried or rerouted by the
product during that unavailable interval.

## Limits and operational choices

No second physical machine was available for this validation. LAN/firewall and
Windows/macOS deployment remain unvalidated. Native inference used llama.cpp;
q27/NInfer/DFlash2/vision remained covered by existing workspace contracts, not
new hardware generations. The fixture rejects embeddings; successful embedding
generation is not claimed. Setup changes intentionally require the relevant
process restart; active/configured state and pending removal are explicit.
