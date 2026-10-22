#!/bin/sh
# Removes Portal installed by install.sh.
#   curl -fsSL https://raw.githubusercontent.com/MStefa003/portal/main/uninstall.sh | sh
# Files you sent or received are not touched.
set -eu

dest="${PORTAL_INSTALL_DIR:-$HOME/.local/bin}"
# The right-click menu entries added by `portal context-menu`, if any.
if [ -x "$dest/portal" ]; then
  "$dest/portal" context-menu --remove >/dev/null 2>&1 || true
fi
rm -f "$dest/portal" "$dest/portal-rendezvous"
echo "Portal has been removed."
echo "Your settings and paired devices are kept; delete ~/.config/portal (Linux) or ~/Library/Application Support/portal (macOS) to remove them too."
