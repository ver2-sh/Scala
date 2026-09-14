# Norted Link dynamic integration validation — 2026-09-14

Inspected clean Norted-Server `e176a07` and Wayfinder `9d31fba`, fetched both
remotes, and confirmed both matched upstream before editing. No changes pushed.

## Final setup and boundary

The Link configuration is exactly `LinkConfig { enabled: bool }`. Enable with
`norted-server link enable`, the Link screen's `e`, or:

```toml
[link]
enabled = true
```

Start Norted beside Wayfinder under the same Linux account/runtime environment.
Norted discovers the public Unix application socket and owns a live registration
for `norted.link.v1`. No descriptor, private-state scan, admin token or persistent
Wayfinder application setup is involved. Disconnection removes registration;
Wayfinder startup/restart causes automatic reconnection and registration.
The peer protocol, routing, runtime manager and adapter behavior are preserved.

## Checks and real integration

Both `./validate.sh` scripts passed formatting, workspace checking, Clippy with
warnings denied and workspace tests. Norted: 214 passed, zero failed, one existing
ignored. Norted all-feature/all-target Clippy passed. Debug and release builds
were produced. `git diff --check` passed in both repositories.

Isolated real processes used fresh Wayfinder identities, separate Norted config,
state and profiles, and an existing llama.cpp binary plus the existing standalone
Grep Qwen3.5 4B GGUF. No model or runtime files were copied. This tests protocol
behavior without changing production profiles or generating benchmark evidence.

- Generic Wayfinder integration: 29 assertions, including unrelated echo service,
  encrypted bidirectional transport, session lifetime, crash/restart recovery,
  peer credentials and application/admin/MCP separation.
- `smoke.py`: 42 assertions in both directions covering owner inventory,
  qualified/unqualified routing, Chat Completions, Responses and Completions in
  streaming and non-streaming forms, terminal/usage behavior, Embeddings capability
  rejection, streaming/non-streaming cancellation, remote load/unload and no JIT.
- `failures.py`: 42 assertions covering invalid source/target/hops/version,
  forbidden protocol operations, bounded frames, exact routing, deterministic
  duplicate aliases, removed owner profiles, interrupted streams, stale peers,
  local inference during Wayfinder loss, reconnect, and unchanged owner stores.
- `lifecycle.py`: 20 assertions covering enabled-only configuration, dynamic
  registration, a third Wayfinder-only node excluded from Norted peers, abrupt
  Norted process death, automatic re-registration after restart, and both
  Wayfinder daemons disappearing and returning without configuration steps.
- Private mount namespaces mask Wayfinder's entire private state directory from
  both Norted processes. Neither can see identity/config/state/control files;
  federation still registers, discovers and serves through the public socket.
  The full 42-check inference smoke suite passed again with both directories hidden.
- TUI federation: 16 action assertions covering remote profile selection,
  load/unload, preserved local backend/profile state and local-only Runtime views.
- Link screen: 12 assertions covering full local IDs, peers, configured/running
  state, simple enable/disable, no path prompt, compact layout and local serving
  while Wayfinder is absent.
- Wayfinder TUI: four assertions for automatic application sockets in owned and
  attached daemon flows and the generic observational Services view.
- Cleanup: 14 assertions confirming federated captures, removal of registrations,
  sockets and descriptors, and closure of isolated public listeners.

Scripts, captures and detailed logs are retained at
`/srv/norted/scratch/norted-link-dynamic/` outside both repositories.

## Actual host config and limits

Appended only `[link] enabled = true` to
`/root/.config/nortedserver/config.toml`. All existing configuration bytes were
preserved, including server/API and model paths. Runtime selections, Model
Profiles, NInfer/DFlash2/Vision/Grep settings and benchmark files were untouched.
Existing host services were not restarted; the new binaries are available for
the one-time version upgrade. Normal later Norted installation/configuration
requires no Wayfinder restart.

The available host Wayfinder is standalone, with no linked physical second node;
no server/gaming-PC physical smoke test is claimed. Windows/macOS dynamic transport
is unsupported. Native inference exercised llama.cpp; q27, NInfer, DFlash2, Vision
and provenance semantics remain covered by their existing workspace contracts,
not new hardware inference runs. The fixture rejects Embeddings; successful
embedding generation is not claimed. No blocking implementation issue was found.
