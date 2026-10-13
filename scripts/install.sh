#!/usr/bin/env bash
# Installs jf-htpc as the boot-to-app session. Idempotent; safe to re-run.
# Never run automatically in dev. Usage:
#   sudo scripts/install.sh [--binary PATH] [--user NAME] [--dry-run]
set -euo pipefail

BIN="target/release/jf-htpc"
HTPC_USER="htpc"
DRY=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary) BIN="$2"; shift 2 ;;
    --user) HTPC_USER="$2"; shift 2 ;;
    --dry-run) DRY=1; shift ;;
    -h|--help) sed -n '2,4p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

run() {
  echo "+ $*"
  if [[ $DRY -eq 0 ]]; then "$@"; fi
}

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UNIT_SRC="$HERE/dist/jf-htpc.service"

if [[ $DRY -eq 0 && $EUID -ne 0 ]]; then
  echo "must run as root (or pass --dry-run)" >&2
  exit 1
fi
[[ -x "$BIN" ]] || { echo "binary not found: $BIN (cargo build --release first)" >&2; exit 1; }

# 1. Dedicated unprivileged user. video/render/input are a fallback in case
#    logind seat ACLs are not applied to the service's session.
if ! id "$HTPC_USER" >/dev/null 2>&1; then
  run useradd --create-home --shell /usr/sbin/nologin "$HTPC_USER"
fi
for g in video render input; do
  if getent group "$g" >/dev/null && ! id -nG "$HTPC_USER" 2>/dev/null | tr ' ' '\n' | grep -qx "$g"; then
    run usermod -aG "$g" "$HTPC_USER"
  fi
done

# 2. Binary and unit. The unit hardcodes User=htpc; rewrite it if overridden.
run install -Dm755 "$BIN" /usr/local/bin/jf-htpc
if [[ "$HTPC_USER" == "htpc" ]]; then
  run install -Dm644 "$UNIT_SRC" /etc/systemd/system/jf-htpc.service
else
  echo "+ install unit with User=$HTPC_USER"
  if [[ $DRY -eq 0 ]]; then
    sed "s/^User=htpc$/User=$HTPC_USER/" "$UNIT_SRC" > /etc/systemd/system/jf-htpc.service
    chmod 644 /etc/systemd/system/jf-htpc.service
  fi
fi

# 3. Text-mode boot with no display manager.
run systemctl daemon-reload
run systemctl set-default multi-user.target
for dm in gdm gdm3 sddm lightdm lxdm xdm greetd ly; do
  if systemctl list-unit-files "$dm.service" --no-legend 2>/dev/null | grep -q "^$dm.service"; then
    run systemctl disable "$dm.service" || true
    run systemctl mask "$dm.service"
  fi
done
run systemctl enable jf-htpc.service

echo
echo "Installed. Reboot, or start now with: systemctl start jf-htpc"
echo "Logs: journalctl -u jf-htpc -f"
echo "To get a login prompt back: systemctl stop jf-htpc && systemctl start getty@tty1"
