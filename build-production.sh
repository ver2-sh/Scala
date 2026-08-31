#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo_root"

printf '==> Building Norted Server (production)\n'
cargo build --release -p norted-server "$@"
printf '==> Binary: %s\n' "$repo_root/target/release/norted-server"
