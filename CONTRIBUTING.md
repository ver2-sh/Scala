# Contributing to Scala

Discuss substantial changes in an issue before opening a focused pull request.
Include the problem, resulting behavior and checks performed. Report security
issues through [SECURITY.md](SECURITY.md).

Use the pinned Rust toolchain in `rust-toolchain.toml` and read [AGENTS.md](AGENTS.md).
From a checkout, run:

```sh
cargo fetch --locked
./validate.sh
```

The existing checks cover formatting, workspace compilation, Clippy and tests.
Keep validation synthetic and cheap; do not load models, run inference, change
benchmark evidence or activate a production service to validate development work.
Preserve standard-artifact portability, settings precedence and equal inference
behavior regardless of model origin. Scala owns the application; Norted model
lineage and Norted-Utils utilities are independent.

For packaging changes, use the pinned cargo-dist and follow
[release maintenance](docs/releases.md). Regenerate dependency notices after locked
dependency changes. Keep lockfile updates scoped, retain upstream notices, and
provide corresponding source for changes to MPL-covered dependencies. Do not put
credentials, local state, models or runtimes in source or release archives.

Contributions to original Scala code are submitted under Apache-2.0, without
changing the licenses or ownership of third-party material. Publication, tags,
repository visibility and hosted validation require separate maintainer approval.
