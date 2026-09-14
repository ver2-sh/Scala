#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
SERVER_DIR="${NORTED_REPOS_DIR:-/srv/norted/repos}/Norted-Server"
UNIT_NAME="norted-server.service"
UNIT_PATH="/etc/systemd/system/${UNIT_NAME}"
GLOBAL_LINK="/usr/local/bin/norted-server"
BINARY="${SERVER_DIR}/target/release/norted-server"

if (( EUID == 0 )) && [[ -n "${SUDO_USER:-}" && "${SUDO_USER}" != "root" ]]; then
  echo "Run this script as your normal user, not with sudo." >&2
  exit 1
fi

if (( EUID == 0 )); then
  SUDO=()
else
  command -v sudo >/dev/null 2>&1 || {
    echo "sudo is required when not running as root." >&2
    exit 1
  }
  SUDO=(sudo)
fi

usage() {
  cat <<EOF
Usage: norted-server <command>

Manage the Norted Server (Rust) as a systemd service.
Also callable as ./norted-server-service.sh <command>.

Commands:
  install   Build Norted Server, install the systemd unit, install the
            global 'norted-server' command, and start the service
  update    Git-pull (ff-only), rebuild, and restart the service
  build     Build the release binary with cargo
  start     Start the service
  stop      Stop the service
  restart   Restart the service
  status    Show service status
  logs      Follow service logs
  disable   Stop and disable the service
  remove    Stop, disable, remove the systemd unit and global command

Environment:
  NORTED_REPOS_DIR  Repository root (default: /srv/norted/repos)
EOF
}

require_server() {
  [[ -d "$SERVER_DIR" && -f "$SERVER_DIR/Cargo.toml" ]] || {
    echo "Norted Server not found at ${SERVER_DIR}." >&2
    echo "Run repos-setup.sh to clone it, or set NORTED_REPOS_DIR." >&2
    exit 1
  }
}

require_cargo() {
  command -v cargo >/dev/null 2>&1 || {
    echo "cargo is not installed; install Rust via rustup." >&2
    exit 1
  }
}

build_server() {
  require_server
  require_cargo
  echo "Building Norted Server (release)..."
  (cd "$SERVER_DIR" && cargo build --release -p norted-server)
  echo "Build complete: ${BINARY}"
}

update_server() {
  require_server
  require_cargo

  echo "Fetching latest changes for Norted-Server..."
  git -C "$SERVER_DIR" fetch --prune origin

  local head_sha upstream_sha upstream
  head_sha="$(git -C "$SERVER_DIR" rev-parse HEAD)"
  upstream="$(git -C "$SERVER_DIR" rev-parse --abbrev-ref --symbolic-full-name '@{upstream}' 2>/dev/null || true)"
  if [[ -z "$upstream" || "$upstream" != origin/* ]]; then
    echo "Current branch has no upstream tracking origin; skipping update." >&2
    exit 1
  fi
  upstream_sha="$(git -C "$SERVER_DIR" rev-parse "$upstream")"

  if [[ "$head_sha" == "$upstream_sha" ]]; then
    echo "Already up to date at ${head_sha:0:12}."
    return 0
  fi

  local dirty
  if ! dirty="$(git -C "$SERVER_DIR" status --porcelain 2>/dev/null)"; then
    echo "Cannot inspect the worktree; skipping update." >&2
    exit 1
  fi
  if [[ -n "$dirty" ]]; then
    echo "Worktree is dirty or has untracked files; refusing to update." >&2
    echo "Commit or stash your changes, then rerun 'norted-server update'." >&2
    exit 1
  fi

  if git -C "$SERVER_DIR" merge-base --is-ancestor HEAD "$upstream"; then
    echo "Fast-forwarding to ${upstream_sha:0:12}..."
    git -C "$SERVER_DIR" merge --ff-only "$upstream"
  elif git -C "$SERVER_DIR" merge-base --is-ancestor "$upstream" HEAD; then
    echo "Local is ahead of ${upstream}; leaving it unchanged." >&2
    return 0
  else
    echo "Local branch has diverged from ${upstream}; refusing to update." >&2
    echo "Rebase or merge manually, then rerun 'norted-server update'." >&2
    exit 1
  fi

  echo
  build_server
  echo
  install_unit --built
  echo "Norted Server updated and restarted."
}

install_unit() {
  if [[ "${1:-}" != --built ]]; then build_server; fi

  local run_user run_group home_dir tmp_unit backup_unit
  local old_unit_exists=0 old_active=0 old_enabled=0 rollback_needed=0

  run_user="$(stat -c '%U' "$SERVER_DIR")"
  run_group="$(stat -c '%G' "$SERVER_DIR")"
  home_dir="$(getent passwd "$run_user" | cut -d: -f6)"
  [[ -n "$home_dir" ]] || {
    echo "Could not determine home directory for ${run_user}." >&2
    exit 1
  }

  # Shared OS contract; membership grants application transport only.
  getent group wayfinder-apps >/dev/null || "${SUDO[@]}" groupadd --system wayfinder-apps

  tmp_unit="$(mktemp)"
  backup_unit="$(mktemp)"
  trap 'rm -f "$tmp_unit" "$backup_unit"' RETURN

  cat > "$tmp_unit" <<EOF
[Unit]
Description=Norted Server (local inference control plane and OpenAI-compatible gateway)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${run_user}
Group=${run_group}
SupplementaryGroups=wayfinder-apps
WorkingDirectory=${SERVER_DIR}
Environment=HOME=${home_dir}
ExecStart=${BINARY} serve
Restart=always
RestartSec=2
NoNewPrivileges=true
PrivateTmp=true
UMask=0077

[Install]
WantedBy=multi-user.target
EOF

  if "${SUDO[@]}" test -e "$UNIT_PATH"; then
    old_unit_exists=1
    "${SUDO[@]}" cat "$UNIT_PATH" > "$backup_unit"
  fi
  if systemctl is-active --quiet "$UNIT_NAME" 2>/dev/null; then
    old_active=1
  fi
  if systemctl is-enabled --quiet "$UNIT_NAME" 2>/dev/null; then
    old_enabled=1
  fi

  rollback_unit() {
    local original_rc="${1:-1}"
    trap - ERR
    set +e

    echo
    echo "Service activation failed; restoring the previous systemd state..." >&2

    if (( old_unit_exists )); then
      "${SUDO[@]}" install -m 0644 "$backup_unit" "$UNIT_PATH"
    else
      "${SUDO[@]}" rm -f "$UNIT_PATH"
    fi
    "${SUDO[@]}" systemctl daemon-reload

    if (( old_enabled )); then
      "${SUDO[@]}" systemctl enable "$UNIT_NAME" >/dev/null 2>&1
    else
      "${SUDO[@]}" systemctl disable "$UNIT_NAME" >/dev/null 2>&1
    fi

    if (( old_active )); then
      "${SUDO[@]}" systemctl restart "$UNIT_NAME"
      if ! systemctl is-active --quiet "$UNIT_NAME"; then
        echo "WARNING: previous service could not be restored to active state automatically." >&2
      fi
    else
      "${SUDO[@]}" systemctl stop "$UNIT_NAME" >/dev/null 2>&1
    fi

    set -e
    exit "$original_rc"
  }

  on_error() {
    local rc=$?
    if (( rollback_needed )); then
      rollback_unit "$rc"
    fi
    exit "$rc"
  }

  trap on_error ERR

  echo "Installing systemd unit..."
  rollback_needed=1
  "${SUDO[@]}" install -m 0644 "$tmp_unit" "$UNIT_PATH"
  "${SUDO[@]}" systemctl daemon-reload
  "${SUDO[@]}" systemctl enable "$UNIT_NAME"

  if (( old_active )); then
    "${SUDO[@]}" systemctl restart "$UNIT_NAME"
  else
    "${SUDO[@]}" systemctl start "$UNIT_NAME"
  fi

  "${SUDO[@]}" systemctl is-active --quiet "$UNIT_NAME"

  rollback_needed=0
  trap - ERR

  # Install the global 'norted-server' symlink so the script is callable from anywhere.
  if [[ -e "$GLOBAL_LINK" && ! -L "$GLOBAL_LINK" ]]; then
    echo "WARNING: ${GLOBAL_LINK} exists but is not a symlink; leaving it unchanged." >&2
  else
    local tmp_link="${ROOT}/.norted-server.new.$$"
    rm -f "$tmp_link"
    ln -s "${ROOT}/norted-server-service.sh" "$tmp_link"
    "${SUDO[@]}" mv -Tf "$tmp_link" "$GLOBAL_LINK"
  fi

  echo
  "${SUDO[@]}" systemctl --no-pager --full status "$UNIT_NAME"
}

cmd="${1:-}"
case "$cmd" in
  install)
    install_unit
    ;;
  update)
    update_server
    ;;
  build)
    build_server
    ;;
  start)
    require_server
    "${SUDO[@]}" systemctl start "$UNIT_NAME"
    ;;
  stop)
    "${SUDO[@]}" systemctl stop "$UNIT_NAME"
    ;;
  restart)
    require_server
    "${SUDO[@]}" systemctl restart "$UNIT_NAME"
    "${SUDO[@]}" systemctl is-active --quiet "$UNIT_NAME"
    ;;
  status)
    "${SUDO[@]}" systemctl --no-pager --full status "$UNIT_NAME"
    ;;
  logs)
    "${SUDO[@]}" journalctl -u "$UNIT_NAME" -n 100 -f
    ;;
  disable)
    "${SUDO[@]}" systemctl disable --now "$UNIT_NAME"
    ;;
  remove)
    "${SUDO[@]}" systemctl disable --now "$UNIT_NAME" 2>/dev/null || true
    "${SUDO[@]}" rm -f "$UNIT_PATH"
    "${SUDO[@]}" systemctl daemon-reload
    # Remove the global symlink only if it points at this script.
    if [[ -L "$GLOBAL_LINK" ]]; then
      local resolved
      resolved="$(readlink -f "$GLOBAL_LINK" 2>/dev/null || true)"
      if [[ "$resolved" == "${ROOT}/norted-server-service.sh" ]]; then
        "${SUDO[@]}" rm -f "$GLOBAL_LINK"
      fi
    fi
    echo "Removed ${UNIT_PATH}. Norted Server source was left untouched."
    ;;
  -h|--help|"")
    usage
    exit 0
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
