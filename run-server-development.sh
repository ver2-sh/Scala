#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo_root"

"$repo_root/build-development.sh"
printf '==> Starting Scala headless (development)\n'
exec "$repo_root/target/debug/scala" serve "$@"
