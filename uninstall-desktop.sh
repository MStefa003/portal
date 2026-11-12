#!/bin/sh
# Removes Portal Desktop installed by install-desktop.sh.
#   curl -fsSL https://raw.githubusercontent.com/MStefa003/portal/main/uninstall-desktop.sh | sh
# The `portal` command line tool, files you sent or received, and settings
# are not touched.
set -eu

case "$(uname -s)" in
  Darwin)
    for apps in "${PORTAL_DESKTOP_APPS_DIR:-/Applications}" "$HOME/Applications"; do
      rm -rf "$apps/Portal.app"
    done
    ;;
  *)
    bin="${PORTAL_INSTALL_DIR:-$HOME/.local/bin}"
    data="${XDG_DATA_HOME:-$HOME/.local/share}"
    rm -f "$bin/portal-desktop" \
      "$data/applications/portal-desktop.desktop" \
      "$data/icons/hicolor/256x256/apps/portal-desktop.png"
    if command -v update-desktop-database >/dev/null; then
      update-desktop-database -q "$data/applications" || true
    fi
    ;;
esac
echo "Portal Desktop has been removed."
echo "Your settings are kept (the command line tool uses them too)."
