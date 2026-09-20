#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
DIST="${DIST:-dist}"
export DIST
python3 scripts/release-version.py "${1:-$(python3 scripts/release-version.py)}"
cargo metadata --locked --format-version=1 --no-deps >/dev/null
./validate.sh
cargo build --locked --profile dist -p norted-server
python3 scripts/dist-workflow.py --check
"$DIST" generate --check
mkdir -p target
"$DIST" plan --output-format=json > target/release-plan.json
python3 - <<'PY'
import json
import tomllib
from pathlib import Path
config = tomllib.loads(Path('dist-workspace.toml').read_text())['dist']
assert config['cargo-dist-version'] == '0.33.0'
assert config['cache-builds'] is False
assert config['merge-tasks'] is True
plan = json.loads(Path('target/release-plan.json').read_text())
assert [release['app_name'] for release in plan['releases']] == ['norted-server']
rows = plan['ci']['github']['artifacts_matrix']['include']
assert {target for row in rows for target in row['targets']} == set(config['targets'])
assert sum(len(row['targets']) for row in rows) == 5
assert plan['ci']['github']['pr_run_mode'] == 'skip'
assert len([row for row in rows if 'macos' in row['runner']]) == 1
PY
if command -v actionlint >/dev/null 2>&1; then actionlint; fi
for script in ./*.sh scripts/*.sh; do bash -n "$script"; done
printf 'Local preflight passed. No tag, push, release, or hosted workflow was created.\n'
