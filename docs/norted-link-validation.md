# Norted Link separate-account validation — 2026-09-14

Inspected clean Norted-Server `24f315b9479a869e0825e74825549b56b136d00e`
and Wayfinder `acd7529662936057c54a7b99bf31f4b72e64f4d0` before editing.
Changes remain local; nothing was pushed.

## Security correction

Requiring the same UID granted Norted normal filesystem access to Wayfinder's
private authority, regardless of application-protocol restrictions. The earlier
mount-namespace validation did not establish ordinary separate-account isolation.

The installed generic endpoint is `/run/wayfinder/app.sock`. Wayfinder owns its
2750 directory, with group `wayfinder-apps`; the socket inherits that group and
has mode 0660. Linux authorizes connections using effective UID and supplementary
groups. Wayfinder retains SO_PEERCRED UID in the ephemeral session owner identity.
Applications cannot replace directory entries. Norted validates protected
endpoint ownership/permissions and verifies the daemon peer UID against the
directory owner, rather than its own UID.

Private state remains 0700, files 0600. Application-group membership does not
permit reading identity.json, config.json, state.json or control.json. The
application decoder still only accepts sanitized status, session-owned
registration/unregistration, and exact-node named-service opening. MCP,
administration, membership mutation and shell execution remain separate.

Both service installers provision the generic group, including on version
updates. Wayfinder uses systemd RuntimeDirectory; Norted receives a supplementary
group. Neither installer creates a service record. Manual accounts need only the
generic OS group grant documented in the Link guide. Wayfinder has no Norted,
AI, GPU or model semantics. Norted Link configuration remains exactly
`LinkConfig { enabled: bool }`.

## Checks

- Both `./validate.sh` scripts passed formatting, workspace check, Clippy with
  warnings denied and workspace tests. Norted: 215 passed, one existing ignored.
  Wayfinder has no Rust unit tests; its real-process suite is reported below.
- Norted all-feature/all-target Clippy passed. Debug binaries were built.
- A new Norted endpoint-trust test rejects group-writable directories,
  world-authorized sockets, directory symlinks and writable runtime parents.
- Both shell scripts passed `bash -n`; both repositories passed `git diff --check`.
- A transient systemd unit using an existing unprivileged account verified that
  RuntimeDirectoryMode=2750 is preserved and the socket inherits the directory
  group under UMask=0077. Production service units were not installed or restarted.
- Removed unused direct `reqwest` and `directories` dependencies from norted-api;
  other crates retain their own required dependencies.

## Actual separate-UID processes

No private mount namespaces hid any files. The test driver remained root solely
to provision isolated directories, launch processes with distinct credentials,
and exercise private administration for test setup.

- Generic suite: daemon UID 61001, application UID 61002 with supplementary GID
  61003; unauthorized UID 61004 without that group. **51 assertions passed**.
  Includes arbitrary `echo.private.v1`, bidirectional encrypted echo across real
  Noise-linked nodes, exact targeting, sanitized fields, third-node absence,
  invalid operation rejection, MCP/admin credential rejection, all four private
  file denials, private modes, unauthorized socket denial, registration ownership,
  crash cleanup, application restart and daemon restart/reconnection.
- Norted A: Wayfinder UID 61101, Norted UID 61102. Norted B: Wayfinder UID 61111,
  Norted UID 61112. Both application processes have supplementary GID 61103.
  Wayfinder-only C uses UID 61121. A uses the actual machine endpoint; B/C use
  protected isolated runtime directories on the same host.
- **42 bidirectional smoke assertions passed**, then passed again on the final
  binary after restart. Covers remote model/profile/runtime inventory, stable
  qualified aliases, Chat Completions, Responses, Completions, streaming,
  terminal/usage responses, streaming and nonstream cancellation, remote
  load/unload, the then-current rejection without remote JIT load (superseded by
  ordinary owner-side JIT admission), and Embeddings capability
  rejection for the fixture.
- **42 failure assertions passed**: invalid source/target/hops/version, forbidden
  operations, bounded framing, exact routing, deterministic duplicate aliases,
  removed profiles, interrupted streams, stale peers, local inference during
  Wayfinder loss, recovery and unchanged authoritative profile stores.
- **45 lifecycle/filesystem assertions passed**: enabled-only configuration,
  actual process UIDs, automatic registration, private-file denial, inability to
  replace the socket, third node excluded from Norted peers, process death and
  restart, ordinary login XDG directory falling back to the machine endpoint,
  local inference without Wayfinder, recreation of `/run/wayfinder`, automatic
  federation recovery, and no model/runtime replication.

Registration still belongs to a live session: EOF/death removes it, application
restart registers anew, and daemon restart triggers automatic reconnection and
registration. No persistent service records, leases or manual crash cleanup.

## Fixtures, host and limitations

The test used fresh identities, config, profiles and state, with the existing
standalone 4B GGUF and existing llama.cpp CUDA runtime. Each owner received an
explicit local copy of the runtime fixture outside root's private home; model
bytes were not copied. No runtime/model files were transferred through federation
or added to the other owner's authoritative stores. Sharing a physical host and
read-only model input is a validation limitation, not a claimed two-machine test.

The host `/root/.config/nortedserver/config.toml` was read and confirmed to have
exactly `[link] enabled = true`; it was not rewritten. Production profiles,
runtime settings and services were not changed. Test processes and the temporary
machine endpoint were removed after validation. Scripts and logs are retained at
`/srv/norted/scratch/norted-link-uids/` outside the repositories.

Linux is the validated platform. Small cfg boundaries preserve unrelated
functionality on unsupported platforms; no Windows/macOS cross-build was run.
q27, NInfer, DFlash2, Vision, local Runtime UI and provenance code was unchanged
and covered by existing workspace checks, not new native inference/UI runs.
Successful embedding generation was not exercised. No remaining Blocking or
Material issue was identified in this integration audit.

## JIT semantics correction — 2026-09-16

Starting from Norted-Server `77cd193`, reachable installed remote profiles are now
listed and routed without requiring a running backend. Forwarded inference uses
the owner's ordinary RuntimeManager admission, including its local JIT policy.
The earlier unloaded-profile rejection above records historical behavior, not the
current intended contract. The retained external smoke script still asserts that
old behavior and was not counted as current validation.

- `./validate.sh` passed formatting, workspace check, Clippy for all targets with
  warnings denied, and workspace tests: 216 passed, one existing ignored.
- `cargo build -p norted-server` and `git diff --check` passed.
- A synthetic API regression test verifies installed/unloaded remote discovery and
  exact routing, deterministic collision aliases, and name reservations with
  rejection/exclusion for missing owner artifacts and stale owner observations.
- Code inspection confirms internal owner requests retain `link: None` and exact
  `EXECUTION_PROFILE` binding across all four inference surfaces. Entry dispatch
  still makes one request; protocol identity/hop checks and explicit load/unload
  are unchanged. No gateway load/poll/retry path was introduced.
- An isolated two-instance setup was attempted using a copy of the retained
  harness under `/srv/norted/scratch/norted-link-jit/`. It stopped before starting
  any daemons: the available Wayfinder binary rejects the harness's `init`
  command. Wayfinder was not changed. Production services and profiles were not
  touched. No real two-instance JIT success, backend reuse, unload/JIT reload or
  JIT-disabled inference result is claimed for this correction. Those live checks
  remain outstanding, as does gaming-PC validation.
