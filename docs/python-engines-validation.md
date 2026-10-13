# Managed Python runtime integration validation — 2026-10-13

The managed-runtime continuation inspected clean local HEAD `35c5ec9fa` (v0.1.21)
and `AGENTS.md` before changes. Changes are confined to Scala. Existing production
configuration, environments, runtime installations, model artifacts, profiles and
services were untouched. All installation tests used isolated temporary stores,
mock HTTP acquisition, fake interpreters/tools and synthetic admission. No tensor
packages were installed into live environments, and no model inference, training,
benchmark or hosted CI/release was run.

| Final local check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo check --workspace --all-targets --offline` | Passed |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | Passed |
| `cargo test --workspace --offline -- --test-threads=1` | 462 passed, 0 failed; 1 existing manual benchmark ignored |
| `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu --offline` | Passed with existing unrelated platform warnings |
| Python integrity/managed provisioning unit discovery | 12 passed |
| Python vLLM/Windows provisioning unit discovery | 15 passed |
| Python PyTorch runner unit discovery | 6 passed |
| `scripts/release-legal.py` | 455 locked dependency notices; 3 MPL source archives; embedded runtime source notices retained |
| `scripts/dist-workflow.py --check` with privately bootstrapped repository-pinned cargo-dist 0.33.0 | Passed; original tag-only workflow retained |
| Local cargo-dist musl archive build | Passed; archive SHA-256 verified |
| Packaged binary copied into an isolated directory | 12 current embedded recipe/runner/lock resources and notices verified; runtime and migration CLI help passed without a checkout |
| `git diff --check` | Passed |

The tests cover both catalog providers and independent Linux/native Windows tuples;
strict artifact/runner qualification and unsupported host targets; privately managed
package-tool acquisition; exact lock/provenance authority; staged activation, failed
admission, cancellation and matching interrupted-install recovery; explicit
side-by-side updates, active leases, immutable ownership and cache cleanup; ordinary
composition registration; engine-specific/model-first searches that avoid unrelated
network providers; explicit legacy-profile review/confirmation and preserved bindings;
and the existing native engines, APIs, Settings and Scala Link contracts.

The final full workspace run is serial. One earlier concurrent run encountered the
unchanged llama.cpp synthetic restart test's benchmark-history owner lease race;
the exact test passed in isolation and the final full serial run passed. No unrelated
benchmark/storage code was modified to hide that failure.

Native Windows code was cross-compiled and checked with existing synthetic source
fixtures; no Windows host or GPU was contacted. Real dependency provisioning and
model/GPU execution qualification were intentionally not performed. Package and
source hashes remain mandatory; a missing/replaced pinned download or unsupported
host fails closed. Installed size remains unknown where complete measurements are
unavailable, and installation does not imply that a model fits memory or has passed
native execution qualification.

The earlier restructuring record below is retained as historical evidence; its
baseline and counts describe the prior implementation.

## Earlier Python engine restructuring validation — 2026-10-13

Inspected local HEAD `fb878b73bffded95856891895fa514b4557c4f4f` before editing;
the working tree was initially clean. Work was confined to Scala and temporary
validation/build artifacts. No production configuration, runtime, model or
persistent profile was changed. No push, deployment, GPU/model inference,
benchmark, training, runtime installation or hosted CI was performed.

## Commands and results

All commands ran from the Scala repository. Final authoritative checks:

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo check --workspace --all-targets --offline` | Passed |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | Passed, no warnings |
| `cargo test --workspace --lib --bins --offline` | 450 passed; 0 failed; 1 existing manual benchmark ignored |
| `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu --offline` | Passed; existing unrelated warnings in startup, installer and Wayfinder code |
| `python3 -B -m unittest discover -s crates/scala-engine/runtime -p 'test_*.py'` | 7 passed |
| `python3 -B -m unittest discover -s crates/scala-engine-vllm/runtime -p 'test_*.py'` | 15 passed |
| `python3 -B -m unittest discover -s crates/scala-engine-pytorch/runtime -p 'test_*.py'` | 6 passed |
| `git diff --check` | Passed |

Focused Rust checks also exercised both new engine crates, Scala's composition
and the public API crate before the complete workspace run. Windows checks compile
the platform-specific launcher and process ownership paths; they do not execute
Windows binaries. Python launch tests use fake children and upstream backends.
The HF cache test verifies ordinary symlink-backed snapshots and identical Rust /
Python content fingerprints without importing tensor libraries.

Coverage includes independent registration/inventories and upstream version
identities; native Chat/Responses routing and fragmented SSE; omitted defaults,
explicit overrides and unsupported controls; H2O trained request/response fidelity;
Imajev trained readout/calibration/fast dispatch and original images; incompatible
runtime/model/runner bindings; discovery cache invalidation and bounded waits;
process cancellation and readiness invalidation; old profile readability and
explicit migration preserving IDs/model/name/role; Decision candidate versus
execution support; and existing API/Scala Link contracts.

Verified byte-for-byte against HEAD that the moved H2O shim, serve configuration,
source review record, Windows serving/packaging locks and Imajev dependency lock
were unchanged. Source descriptors and real model artifacts were not rewritten. Historical Scala-owned
Linux runtime JSON metadata was read without probing/execution to confirm the
original torch/torchvision/transformers/PEFT tuples; both installed environments
were untouched.

## Operational qualification still required

Live Linux and native Windows GPU loading/forward execution, CUDA/SDK/JIT startup
and accelerated-mode numerical behavior require an isolated operational check
against the chosen exact runtime/model/settings tuple. They were not tested here.
There is no claimed speedup or GPU qualification from these development tests.
Merged LoRA serving remains explicitly unsupported until independently calibrated
and qualified. Source integration has no known remaining technical blocker.

See [supported engines and explicit migration](python-engines.md). The optional
`isolated_python_engines` example and `scripts/qualify-python-engines.py` are for
explicit future operational qualification in a new state tree; they were not run.
