# Upstream license supplements

These are unchanged upstream files missing from published dependency crates.
`manifest.json` binds each package/version to a source URL and SHA-256. Most URLs
use the revision in the crate's `.cargo_vcs_info.json`. The two winapi target
support crates omit that metadata; their repository licenses are pinned to the
winapi 0.3.9 tag commit. rustls-platform-verifier-android also omits VCS metadata;
its repository licenses are pinned to the parent verifier 0.7.0 release commit.
These exceptions preserve actual upstream notices without inventing copyright.

`scripts/release-legal.py` verifies these hashes and combines the files with
license, notice, copyright and author files from checksum-verified Cargo.lock
crate archives. It fails for missing licenses, unexpected dependency sources or
stale supplements. Regeneration uses `cargo fetch --locked` followed by
`python3 scripts/release-legal.py`; generation itself is offline. The output is
`target/release-legal`, included by cargo-dist in each application archive.

The bundle covers all locked registry dependencies, including build/dev and other
platform dependencies; it is not a claim that every crate is linked on every
platform. No dependency license is replaced by Scala's Apache-2.0 license.
