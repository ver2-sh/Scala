# Scala release maintenance

Scala (`ver2-sh/Scala`) owns the `scala` executable, `scala-*` workspace crates,
`scala.service`, platform-native Scala storage, and Scala Link (`scala.link.v1`).
Its standard host checkout is `/srv/norted/repos/Scala`. The distribution contains
only the application; models, inference runtimes and GPU drivers are separate.
Norted owns model construction and immutable artifact lineage; Norted-Utils owns
shared host preparation. Neither acquires a Scala-specific artifact contract.

## Channel status

**PRIVATE / UNPUBLISHED:** the repository is private. There is no anonymously accessible release channel yet; the installer URLs below are not live consumer URLs.
A draft is not a published release; its presence does not make installer URLs work.
Do not make the repository public or publish tags as a validation technique.
The existing repository policy permits local checks, not hosted validation runs.
Actual publication and any change of visibility require deliberate approval.

The workflow will publish **Scala v0.1.0** when the matching
reviewed version tag is intentionally pushed. It is tag-only: ordinary branch
pushes, pull requests and schedules do not consume Actions minutes.

## What is shipped

| Packaging target | Intended platform | Inference support |
| --- | --- | --- |
| `x86_64-unknown-linux-musl` | Linux x64; includes x64 WSL | Exact runtime/host admission |
| `aarch64-unknown-linux-musl` | Linux ARM64 | Exact runtime/host admission |
| `aarch64-apple-darwin` | macOS Apple Silicon | Exact runtime/host admission |
| `x86_64-apple-darwin` | macOS Intel | Exact runtime/host admission |
| `x86_64-pc-windows-msvc` | Windows x64 | Exact runtime/host admission |

Every archive contains the `scala` binary (`.exe` on Windows), `LICENSE`, `NOTICE`,
and `release-legal/` dependency notices, source hashes and MPL source archives. cargo-dist
also produces shell and PowerShell installers, per-archive SHA-256 checksums,
`sha256.sum`, and a distribution manifest. No models, runtimes, GPU drivers,
credentials, local configuration, benchmark evidence or Rust toolchain is bundled.
These are server build targets, not a promise that NInfer/q27/llama.cpp have equal
native support on every platform. Existing runtime installation and compatibility
checks remain authoritative; equivalent non-Norted artifacts receive equal behavior.

No new background service, updater daemon, telemetry, domain, package repository,
MSI or signing identity is provisioned. Source-service administration remains
separate from release-binary installation.

## Install and update

These anonymous URLs become usable only after an approved public release exists.
`ver2.sh` is Scala's stable distribution front dor. Its dedicated Cloudflare
Worker serves the installer bodies and versioned release assets under
`https://ver2.sh/scala/`, while the Scala release workflow remains the publishing
authority. The Worker accepts no GitHub credentials and rewrites installer archive
downloads back through the versioned `ver2.sh` route.

The generated cargo-dist installers remain authoritative. Latest stable one-liners:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://ver2.sh/scala/install.sh | sh
```

```powershell
irm https://ver2.sh/scala/install.ps1 | iex
```

For an inspect-first, reproducible installation, use the versioned URLs below
instead of `latest`. `v0.1.0` is the planned version, not a claim of publication;
replace it only with an intentionally published version.

Linux/macOS:

```sh
curl --proto '=https' --tlsv1.2 -fLsS \
  https://ver2.sh/scala/releases/download/v0.1.0/scala-installer.sh \
  -o scala-installer.sh
# Inspect the downloaded script, then:
sh scala-installer.sh
# Open a fresh shell if the installer changed PATH.
scala --version
scala doctor
scala tui
# For foreground headless serving instead:
scala serve
```

Windows PowerShell:

```powershell
Invoke-WebRequest -Uri 'https://ver2.sh/scala/releases/download/v0.1.0/scala-installer.ps1' -OutFile 'scala-installer.ps1'
# Inspect the downloaded script, then:
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scala-installer.ps1
# Open a fresh shell if the installer changed PATH.
scala --version
scala doctor
scala tui
```

The PowerShell policy is process-scoped; organizational Group Policy still wins.
Do not disable Gatekeeper, SmartScreen or managed execution policy.

Application updates:

```sh
scala update --check
scala update
scala update --yes
scala --json update --check
scala --json update --yes
```

`--check` always attempts fresh stable discovery. `update` reports the current and
latest stable versions and requires typing `yes` before replacement. `--yes` is
explicit unattended approval; JSON and noninteractive execution never prompt.
`--check --yes` is rejected. An inaccessible/private/unpublished channel returns a
failure, never stale cached success. Only a strictly newer stable version is installed.

The shared `scala-update` crate uses axoupdater **0.10.2**, semver and the same
reqwest 0.13 rustls alias as Wayfinder. Discovery is fixed to `ver2-sh/Scala`, app
`scala`, independently of Link, gateway or application configuration. Environment
source/receipt overrides are rejected; no private token is requested or supplied.
The updater dispatches before application configuration, discovery and inference.

A matching cargo-dist receipt (publisher, app, installed version, binary, prefix
and the configured cargo-home layout) plus axoupdater's executable ownership check
are required. Missing/mismatched
receipts, source/manual copies and package-manager installations must use their
original installation method. No package-manager channel is provisioned here.
The exact release discovered before approval is retained for installation through
its generated installer; Scala adds no download protocol or updater daemon.

Stop existing Scala serving/TUI and control instances before replacement, then
restart using the original invocation/service method afterward. Unlike Wayfinder,
Scala has no native user-service manager: the updater never stops services, kills
processes, interrupts inference or automatically restarts anything. Cross-process
locks serialize replacement against the executable, receipt, install prefix,
application storage and server startup/ownership checks. Application sessions hold
a shared executable lock before loading configuration (also locking the executable
file to cover hard-link aliases and other OS accounts); its per-user anchor ignores
XDG state overrides so different application directories cannot bypass it. These
small persistent locks live in the default Scala state namespace (`~/.local/state/scala/update-locks`
on Linux, `~/Library/Application Support/Scala/update-locks` on macOS,
`~/AppData/Local/Scala/update-locks` on Windows). Do not delete lock files while Scala runs.
Older processes are also guarded through existing storage leases and recorded
server PIDs for the selected state directory. Stop any older copies using other
state directories before updating; they predate the executable lock protocol.

The interactive TUI checks asynchronously on startup, at most daily, including
cached errors. Cache writes are atomic, networking has a 15-second check timeout,
concurrent checks are deduplicated, and future timestamps expire after clock rollback.
The footer retains an update/error indicator; `/update` performs a fresh check and
shows versions/details. Exiting the UI cancels background checks. Headless
serve/status/doctor never automatically check. No telemetry is introduced.

Windows replacement uses upstream process-scoped PowerShell `-ExecutionPolicy Bypass`
and rename/restore support. It does not change persistent execution policy;
MachinePolicy/UserPolicy remain authoritative. Policy failures are reported,
with upstream restoring the previous executable on installer failure. The updater
captures installer output so JSON remains machine readable. Discovery and HTTP
requests time out; replacement itself is not cancelled mid-rename/restore.
Native failure/recovery validation remains a release gate.

Models, runtimes, settings, credentials, operational state and benchmark evidence
are not updated or deleted. Application updates are separate from inference
runtime updates. `scala-service.sh update` remains exclusively a source-checkout
rebuild helper; it must not wrap or overwrite a release installation.

While the repository is private, maintainers can retrieve release assets with
GitHub CLI instead. For example, for Linux x64:

```sh
gh release download v0.1.0 -R ver2-sh/Scala \
  -p scala-x86_64-unknown-linux-musl.tar.xz \
  -p scala-x86_64-unknown-linux-musl.tar.xz.sha256
sha256sum -c scala-x86_64-unknown-linux-musl.tar.xz.sha256
tar -xJf scala-x86_64-unknown-linux-musl.tar.xz
# Copy the extracted executable into a user-owned PATH directory.
```

Private draft downloads require appropriate repository access. Generated anonymous
installers do not inherit GitHub CLI authentication; do not embed a private token
in an installer or send one through a distribution proxy. Never claim a private
release is an anonymous consumer download.

## Local preflight

Use Python 3.11+, the repository Rust toolchain and upstream cargo-dist **0.33.0**:

```sh
DIST=/path/to/dist ./scripts/release-preflight.sh
# Native Linux x64 packaging (requires musl-tools and this Rust target):
rustup target add x86_64-unknown-linux-musl
/path/to/dist build --artifacts=local --target=x86_64-unknown-linux-musl
```

Preflight runs the existing workspace formatting/check/Clippy/test validation,
a locked distribution-profile build, exact stable-version validation, full reproducible workflow
comparison, upstream configuration/plan checks and shell syntax checks. For a
complete five-target build, apply `python3 scripts/dist-installers.py <manifest>`
after global generation, then pass `DIST_MANIFEST=<manifest>` to preflight to
verify the shipping PowerShell script against its actual archives. Local preflight
without that manifest explicitly leaves final Windows artifact verification open. No new
model training, inference benchmark, test suite or production activation is added.
Run in an isolated worktree when a source-built production service is live, so
building does not replace its executable. Preserve benchmark logs and model data.

`dist-workspace.toml` opts in only the server binary. Do not package local target
or configuration directories apart from the generated `target/release-legal` bundle. Never publish artifacts from `dist --artifacts=lies`:
that mode generates intentionally fake archives. Record which native archives were
actually built; a five-target plan alone is not five-platform build verification.

## Intentional publication

1. Set the workspace version in `Cargo.toml`; refresh `Cargo.lock` with Cargo.
   Update `CHANGELOG.md` and confirm the release name in `scripts/dist-workflow.py`.
2. Run `python3 scripts/dist-workflow.py`, then the local preflight. Review the
   source, release contents and platform limitations. Commit through normal review.
3. After release approval, tag the reviewed commit and push that single tag:
   `git tag v0.1.0 <reviewed-commit>` then `git push origin v0.1.0`.
4. The plan job rejects tags not exactly matching the stable workspace version.
   Four build jobs produce five native targets, followed by installer/checksum
   assembly and a single release-publishing job. Only that final job has write
   permission and only publishing steps receive `GH_TOKEN`.
5. Verify the published asset set, checksums and native install/launch behavior.
   A draft prepared manually must be removed or published separately before a tag
   workflow attempts to create the same release; never overwrite reviewed assets.

No PR/branch/manual/scheduled release triggers, persistent build caches, repeated
native validation jobs, or empty announce job are added. Both macOS architectures
share one macOS runner. Linux uses native runners, not hosted cross-toolchain setup.

## Trust and platform gates

Packaging uses pinned upstream cargo-dist, not a custom archive/installer protocol.
`scripts/dist-workflow.py` regenerates upstream YAML and applies only the scoped
release trigger, read-only planning, publisher permission, verified bootstrap,
Scala title, existing-tag guard, dependency notices and installer hash check. `allow-dirty = ["ci"]` permits this upstream
customization; preflight separately compares the complete generated workflow.

`scripts/dist-bootstrap.py` verifies downloaded cargo-dist executable archives
against repository-pinned SHA-256 hashes before reading the executable. Pins come
from the exact upstream 0.33.0 release, as used by Wayfinder. Every job bootstraps
independently; no executable is trusted merely because another job uploaded it.
Version and hashes must be reviewed together when upgrading.

The release channel trusts HTTPS and the publisher. SHA-256 checksums detect
corruption; they are not independent publisher signatures. The cargo-dist 0.33.0
shell installer checks embedded archive hashes. Upstream PowerShell still lacks
this capability ([upstream issue](https://github.com/axodotdev/cargo-dist/issues/2397)).
After global generation, `scripts/dist-installers.py` embeds the selected archive's
SHA-256 from the dist manifest, first checking it against the actual built archive.
The generated `Get-FileHash -LiteralPath ... -Algorithm SHA256 -ErrorAction Stop`
check throws before extraction on missing hashes, read errors or mismatches.
Generation fails on unexpected template shapes or missing archive evidence. PATH,
receipt, install location and replacement behavior remain cargo-dist's own.
The workflow applies and checks this transformation before uploading artifacts.

Source-publication readiness is separate from binary-platform support. Licensing,
notices, the dependency assessment and source checks support developer review;
they do not authorize visibility changes or publication. Local Linux x64 validation
uses actual built archives and unchanged installers/updater through a disposable
HTTPS fixture. Fixture trust is child-process-local and fixture artifacts must
never be distributed. This validates installation/update without a public release.

All five packaging targets remain configured. Linux ARM64, native macOS and
Windows installation/execution are not validated here. Linux Rust cross-checks
and portable PowerShell integrity exercises do not establish Windows behavior.
Authenticode, Apple signing/notarization and GitHub attestations are not enabled.
Before advertising a platform as supported, validate direct install, newer stable
update, active-session refusal, spaced paths, receipt mismatch, failure recovery
and unchanged user data on that native host. Windows also needs normal/managed
PowerShell policy and replacement/rename-restoration validation. Signing policy
and credentials require a separate decision; do not bypass OS security controls.

## Licensing and dependency assessment

Original Scala code uses Apache-2.0, matching Wayfinder's workspace licensing
model. Attribution is in `NOTICE`; dependency owners and licenses are unchanged.
`cargo fetch --locked` then `python3 scripts/release-legal.py` generates the
`target/release-legal` directory from exact checksum-verified locked crate archives
and pinned actual upstream license files in `release/legal-upstream`. Generation
is deterministic and offline, covers all locked dependencies conservatively, and
fails on missing information. It runs before each platform's archive build.

`THIRD-PARTY-NOTICES.txt` retains upstream licenses/attribution; `DEPENDENCIES.json`
records versions, source URLs and hashes. `mpl-sources/` contains unmodified source
.crate archives for option-ext, smartstring and the optionally MPL-licensed termina.
These are gzip tar archives containing the preferred source form and license
notices. option-ext and smartstring retain MPL terms; termina is used under its
MIT option. Preserve corresponding-source availability when redistributing and
provide modifications to MPL-covered files under MPL, per the
[Mozilla guidance](https://www.mozilla.org/en-US/MPL/2.0/FAQ/#q8-i-want-to-distribute-outside-my-organization-executable-programs-or-libraries-that-i-have-compiled-from-someone-elses-unchanged-mpl-licensed-source-code-either-standalone-or-part-of-a-larger-work-what-do-i-have-to-do).

The 2026-09-20 assessment used official cargo-audit 0.22.2, verified against the
upstream GitHub release asset SHA-256, and RustSec advisory database commit
`d5c17953a895cf19e8d3ce66eaa42b6fcfe1fb16`. The only vulnerable locked package was
rustls 0.23.43 ([RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285));
the narrowly scoped 0.23.45 update leaves zero reported vulnerabilities.
[RUSTSEC-2026-0249](https://rustsec.org/advisories/RUSTSEC-2026-0249) remains:
smartstring 1.0.1 is unmaintained and is required by Rhai 1.26.0. This is a maintenance
warning, with no patched version or reported exploit in that advisory. Track
Rhai's migration upstream; replacing the scripting dependency here would be an
unrelated functional change. There are no advisory suppressions. Rerun cargo-audit
with a fresh database before an eventual release; this is a dated assessment.
