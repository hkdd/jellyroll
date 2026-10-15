#!/usr/bin/env bash
# Undoes scripts/install.sh: stops and removes the service, puts the default
# boot target back (graphical.target if a display manager is installed), and
# removes the polkit rule and binary. The jellyroll user and its data
# (sign-in, caches, config) are kept unless --purge. Usage:
#   sudo scripts/uninstall.sh [--user NAME] [--purge] [--dry-run]
set -euo pipefail

APP_USER="jellyroll"
PURGE=0
DRY=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --user) APP_USER="$2"; shift 2 ;;
    --purge) PURGE=1; shift ;;
    --dry-run) DRY=1; shift ;;
    -h|--help) sed -n '2,6p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

run() {
  echo "+ $*"
  if [[ $DRY -eq 0 ]]; then "$@"; fi
}

if [[ $DRY -eq 0 && $EUID -ne 0 ]]; then
  echo "must run as root (or pass --dry-run)" >&2
  exit 1
fi

STATE_DIR=/var/lib/jellyroll-install

# 1. The service.
run systemctl disable --now jellyroll.service || true
run rm -f /etc/systemd/system/jellyroll.service
run rm -f /etc/polkit-1/rules.d/50-jellyroll.rules
run rm -f /usr/local/bin/jellyroll
run systemctl daemon-reload

# 2. Display managers: earlier install.sh versions masked them; enable the
#    first one installed again (this also restores the display-manager.service
#    link that graphical.target starts).
dm_found=""
for dm in gdm gdm3 sddm lightdm lxdm xdm greetd ly; do
  state="$(systemctl is-enabled "$dm.service" 2>/dev/null || true)"
  [[ -z "$state" || "$state" == "not-found" ]] && continue
  if [[ "$state" == masked ]]; then
    run systemctl unmask "$dm.service"
  fi
  if [[ -z "$dm_found" ]]; then
    dm_found="$dm"
    run systemctl enable --force "$dm.service" || true
  fi
done

# 3. The default target from before install.sh, else graphical.target when
#    there is a display manager.
target=""
if [[ -r "$STATE_DIR/default-target" ]]; then
  target="$(cat "$STATE_DIR/default-target")"
elif [[ -n "$dm_found" ]]; then
  target=graphical.target
fi
if [[ -n "$target" ]]; then
  run systemctl set-default "$target"
fi
run rm -rf "$STATE_DIR"

# 4. The login prompt on tty1 (the service stopped it).
run systemctl start getty@tty1.service || true

# 5. Data, only when asked.
if [[ $PURGE -eq 1 ]] && id "$APP_USER" >/dev/null 2>&1; then
  run userdel --remove "$APP_USER"
fi

echo
echo "Uninstalled. Default target: $(systemctl get-default 2>/dev/null || echo unknown)${dm_found:+, display manager: $dm_found}"
if [[ -n "$dm_found" ]]; then
  echo "Desktop now: systemctl isolate graphical.target (or reboot)"
fi
if [[ $PURGE -eq 0 ]]; then
  echo "Kept the $APP_USER user and its data; --purge removes them."
fi
