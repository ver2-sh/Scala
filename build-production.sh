#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo_root"

printf '==> Building Scala (production)\n'
cargo build --release -p scala "$@"
printf '==> Binary: %s\n' "$repo_root/target/release/scala"
