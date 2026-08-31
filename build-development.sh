#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo_root"

printf '==> Building Norted Server (development)\n'
printf '==> For production usage, normally use build-production.sh\n'
cargo build -p norted-server "$@"
printf '==> Binary: %s\n' "$repo_root/target/debug/norted-server"
