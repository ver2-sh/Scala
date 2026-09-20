# Scala release maintenance

Scala (`ver2-sh/Scala`) owns the `scala` executable, `scala-*` workspace crates,
`scala.service`, platform-native Scala storage, and Scala Link (`scala.link.v1`).
Its standard host checkout is `/srv/norted/repos/Scala`. The distribution contains
only the application; models, inference runtimes and GPU drivers are separate.
Norted owns model construction and immutable artifact lineage; Norted-Utils owns
shared host preparation. Neither acquires a Scala-specific artifact contract.

## Channel status

The repository is private. There is no anonymously accessible release channel yet.
A draft is not a published release; its presence does not make installer URLs work.
Do not make the repository public or publish tags as a validation technique.
The existing repository policy permits local checks, not hosted validation runs.
Actual publication and any change of visibility require deliberate approval.

The workflow will publish **Scala v0.1.0** when the matching
reviewed version tag is intentionally pushed. It is tag-only: ordinary branch
pushes, pull requests and schedules do not consume Actions minutes.

## What is shipped

| Archive target | Server platform | Inference support |
| --- | --- | --- |
| `x86_64-unknown-linux-musl` | Linux x64; includes x64 WSL | Exact runtime/host admission |
| `aarch64-unknown-linux-musl` | Linux ARM64 | Exact runtime/host admission |
| `aarch64-apple-darwin` | macOS Apple Silicon | Exact runtime/host admission |
| `x86_64-apple-darwin` | macOS Intel | Exact runtime/host admission |
| `x86_64-pc-windows-msvc` | Windows x64 | Exact runtime/host admission |

Every archive contains the `scala` binary (`.exe` on Windows). cargo-dist
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
For a reproducible installation, use the versioned URLs instead of `latest`.

Linux/macOS:

```sh
curl --proto '=https' --tlsv1.2 -fLsS \
  https://github.com/ver2-sh/Scala/releases/download/v0.1.0/scala-installer.sh \
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
Invoke-WebRequest -Uri 'https://github.com/ver2-sh/Scala/releases/download/v0.1.0/scala-installer.ps1' -OutFile 'scala-installer.ps1'
# Inspect the downloaded script, then:
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scala-installer.ps1
# Open a fresh shell if the installer changed PATH.
scala --version
scala doctor
scala tui
```

The PowerShell policy is process-scoped; organizational Group Policy still wins.
Do not disable Gatekeeper, SmartScreen or managed execution policy.

To update a direct installation, stop its serving/TUI processes and rerun the
installer from the intended newer release. Then restart the same invocation or
service. This replaces the binary, not models, runtimes or configuration. There is
no native `scala update` command in the release binary. The repository's
`scala-service.sh update` is exclusively a source-checkout rebuild helper;
it must not be installed over the release executable as a command wrapper.

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
comparison, upstream configuration/plan checks and shell syntax checks. No new
model training, inference benchmark, test suite or production activation is added.
Run in an isolated worktree when a source-built production service is live, so
building does not replace its executable. Preserve benchmark logs and model data.

`dist-workspace.toml` opts in only the server binary. Do not package local target
or configuration directories. Never publish artifacts from `dist --artifacts=lies`:
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
Scala title and existing-tag guard. `allow-dirty = ["ci"]` permits this upstream
customization; preflight separately compares the complete generated workflow.

`scripts/dist-bootstrap.py` verifies downloaded cargo-dist executable archives
against repository-pinned SHA-256 hashes before reading the executable. Pins come
from the exact upstream 0.33.0 release, as used by Wayfinder. Every job bootstraps
independently; no executable is trusted merely because another job uploaded it.
Version and hashes must be reviewed together when upgrading.

The release channel trusts HTTPS and the publisher. SHA-256 checksums detect
corruption; they are not independent publisher signatures. The cargo-dist 0.33.0
shell installer checks embedded archive hashes. Its PowerShell installer does
**not** verify archive hashes: on Windows verify the downloaded archive with
`Get-FileHash -Algorithm SHA256` against the published checksum before extracting
when explicit hash verification is required. Do not claim automatic Windows hash
verification, Authenticode, notarization or GitHub attestations are enabled.

Native macOS and Windows execution and signing are not established by Linux Rust
cross-checks. Apple Developer and Windows code-signing credentials are not present
in this release setup; native validation and signing readiness remain explicit
public-readiness gates. Use upstream signing support when credentials are supplied,
not custom bypasses. Repository visibility and licensing also require owner review
before exposing this currently private source and its history.
