#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo_root"

printf '==> Building Scala (development)\n'
printf '==> For production usage, normally use build-production.sh\n'
cargo build -p scala "$@"
printf '==> Binary: %s\n' "$repo_root/target/debug/scala"
