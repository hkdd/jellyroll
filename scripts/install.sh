#!/usr/bin/env bash
# Installs jellyroll as the boot-to-app session. Idempotent; safe to re-run.
# Never run automatically in dev. Usage:
#   sudo scripts/install.sh [--binary PATH] [--user NAME] [--dry-run]
set -euo pipefail

BIN="target/release/jellyroll"
APP_USER="jellyroll"
DRY=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary) BIN="$2"; shift 2 ;;
    --user) APP_USER="$2"; shift 2 ;;
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
UNIT_SRC="$HERE/dist/jellyroll.service"

if [[ $DRY -eq 0 && $EUID -ne 0 ]]; then
  echo "must run as root (or pass --dry-run)" >&2
  exit 1
fi
[[ -x "$BIN" ]] || { echo "binary not found: $BIN (cargo build --release first)" >&2; exit 1; }

# 1. Dedicated unprivileged user. video/render/input are a fallback in case
#    logind seat ACLs are not applied to the service's session.
if ! id "$APP_USER" >/dev/null 2>&1; then
  run useradd --create-home --shell /usr/sbin/nologin "$APP_USER"
fi
for g in video render input; do
  if getent group "$g" >/dev/null && ! id -nG "$APP_USER" 2>/dev/null | tr ' ' '\n' | grep -qx "$g"; then
    run usermod -aG "$g" "$APP_USER"
  fi
done

# 2. Binary and unit. The unit hardcodes User=jellyroll; rewrite it if overridden.
run install -Dm755 "$BIN" /usr/local/bin/jellyroll
APP_HOME="$(getent passwd "$APP_USER" | cut -d: -f6 || true)"
APP_HOME="${APP_HOME:-/home/$APP_USER}"
if [[ "$APP_USER" == "jellyroll" && "$APP_HOME" == "/home/jellyroll" ]]; then
  run install -Dm644 "$UNIT_SRC" /etc/systemd/system/jellyroll.service
else
  echo "+ install unit with User=$APP_USER, home $APP_HOME"
  if [[ $DRY -eq 0 ]]; then
    sed -e "s/^User=jellyroll$/User=$APP_USER/" -e "s#/home/jellyroll#$APP_HOME#g" \
      "$UNIT_SRC" > /etc/systemd/system/jellyroll.service
    chmod 644 /etc/systemd/system/jellyroll.service
  fi
fi
# The unit makes the filesystem read-only except these (they must exist).
for d in "$APP_HOME/.local" "$APP_HOME/.local/share" "$APP_HOME/.local/share/jellyroll" \
         "$APP_HOME/.cache" "$APP_HOME/.cache/jellyroll"; do
  [[ -d "$d" ]] || run install -d -m 755 -o "$APP_USER" -g "$APP_USER" "$d"
done

# 2b. Power menu: let the user restart/shut down without a password
#     (logind via polkit; spec 5.5).
if [[ -d /etc/polkit-1/rules.d || $DRY -eq 1 ]]; then
  echo "+ install polkit rule for $APP_USER"
  if [[ $DRY -eq 0 ]]; then
    sed "s/subject.user == \"jellyroll\"/subject.user == \"$APP_USER\"/" \
      "$HERE/docs/polkit-jellyroll.rules" > /etc/polkit-1/rules.d/50-jellyroll.rules
    chmod 644 /etc/polkit-1/rules.d/50-jellyroll.rules
  fi
else
  echo "polkit rules.d not found; power menu may ask for authorization (see docs/polkit-jellyroll.rules)"
fi

# 3. Example config, only if the user has none (never overwrite edits).
CFG_DIR="$APP_HOME/.config/jellyroll"
if [[ ! -e "$CFG_DIR/config.toml" ]]; then
  run install -d -m 755 -o "$APP_USER" -g "$APP_USER" "$APP_HOME/.config" "$CFG_DIR"
  run install -m 644 -o "$APP_USER" -g "$APP_USER" "$HERE/dist/config.example.toml" "$CFG_DIR/config.toml"
fi

# 4. Text-mode boot with no display manager.
run systemctl daemon-reload
run systemctl set-default multi-user.target
for dm in gdm gdm3 sddm lightdm lxdm xdm greetd ly; do
  if systemctl list-unit-files "$dm.service" --no-legend 2>/dev/null | grep -q "^$dm.service"; then
    run systemctl disable "$dm.service" || true
    run systemctl mask "$dm.service"
  fi
done
run systemctl enable jellyroll.service

echo
echo "Installed. Reboot, or start now with: systemctl start jellyroll"
echo "Config: $CFG_DIR/config.toml"
echo "Logs: journalctl -u jellyroll -f"
echo "To get a login prompt back: systemctl stop jellyroll && systemctl start getty@tty1"
