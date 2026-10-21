#!/bin/sh
# Removes Portal installed by install.sh.
#   curl -fsSL https://raw.githubusercontent.com/MStefa003/portal/main/uninstall.sh | sh
# Files you sent or received are not touched.
set -eu

dest="${PORTAL_INSTALL_DIR:-$HOME/.local/bin}"
rm -f "$dest/portal" "$dest/portal-rendezvous"
echo "Portal has been removed."
