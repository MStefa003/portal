#!/bin/sh
# Installs the latest Portal Desktop release on macOS or Linux: the app with
# a window, for sending and receiving without the command line.
#   curl -fsSL https://raw.githubusercontent.com/MStefa003/portal/main/install-desktop.sh | sh
# It does not install the `portal` command line tool (install.sh does), and
# that one does not install this. Remove it with uninstall-desktop.sh.
set -eu

repo="MStefa003/portal"

case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) echo "Unsupported CPU: $(uname -m)" >&2; exit 1 ;;
esac
case "$(uname -s)" in
  Darwin) target="$arch-apple-darwin" ext=dmg ;;
  Linux) target="$arch-unknown-linux-gnu" ext=tar.gz ;;
  *) echo "Unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac

# PORTAL_VERSION (e.g. v0.6.0) installs that release instead of the newest;
# PORTAL_DOWNLOAD_URL takes the files from another place (a mirror, or a
# test build).
if [ -n "${PORTAL_VERSION:-}" ]; then
  tag=$PORTAL_VERSION
else
  # The newest version, from where github.com/<repo>/releases/latest
  # redirects. Unlike the GitHub API, this has no hourly limit per IP address.
  latest=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest" || true)
  tag=${latest##*/releases/tag/}
  if [ -z "$latest" ] || [ "$tag" = "$latest" ]; then
    echo "Could not find a Portal release to install. See https://github.com/$repo/releases" >&2
    exit 1
  fi
fi
name="portal-desktop-$tag-$target.$ext"
base="${PORTAL_DOWNLOAD_URL:-https://github.com/$repo/releases/download/$tag}"
url="${base%/}/$name"

tmp=$(mktemp -d)
mounted=""
cleanup() {
  if [ -n "$mounted" ]; then hdiutil detach -quiet "$mounted" || true; fi
  rm -rf "$tmp"
}
trap cleanup EXIT
file="$tmp/$name"
echo "Downloading $url"
if ! curl -fsSL "$url" -o "$file"; then
  echo "Portal $tag has no desktop app for $target yet. See https://github.com/$repo/releases" >&2
  exit 1
fi
expected=$(curl -fsSL "$url.sha256" | cut -d ' ' -f 1)
if command -v sha256sum >/dev/null; then
  actual=$(sha256sum "$file" | cut -d ' ' -f 1)
else
  actual=$(shasum -a 256 "$file" | cut -d ' ' -f 1)
fi
if [ "$expected" != "$actual" ]; then
  echo "Checksum mismatch." >&2
  exit 1
fi
# With minisign installed, also check that Portal's release key signed it.
release_key=RWTIdVXMW8Z0RQixMzNSOsaChfzseZPChiIB/jxRwiB7PD9hBKs33lhi
if command -v minisign >/dev/null; then
  curl -fsSL "$url.minisig" -o "$file.minisig"
  if ! minisign -Vm "$file" -x "$file.minisig" -P "$release_key" |
    grep -qx "Trusted comment: file:$name"; then
    echo "The download is not signed by Portal's release key." >&2
    exit 1
  fi
  echo "Signature verified."
fi

if [ "$ext" = dmg ]; then
  # Into /Applications when this user may write there (admin accounts can),
  # else ~/Applications.
  apps="${PORTAL_DESKTOP_APPS_DIR:-/Applications}"
  if [ ! -w "$apps" ]; then
    apps="$HOME/Applications"
    mkdir -p "$apps"
  fi
  mounted="$tmp/mnt"
  hdiutil attach -nobrowse -readonly -quiet -mountpoint "$mounted" "$file"
  rm -rf "$apps/Portal.app"
  ditto "$mounted/Portal.app" "$apps/Portal.app"
  echo "Installed: $apps/Portal.app"
  open "$apps/Portal.app"
else
  bin="${PORTAL_INSTALL_DIR:-$HOME/.local/bin}"
  data="${XDG_DATA_HOME:-$HOME/.local/share}"
  tar xzf "$file" -C "$tmp"
  dir=$(echo "$tmp"/portal-desktop-*/)
  mkdir -p "$bin" "$data/applications" "$data/icons/hicolor/256x256/apps"
  install -m 755 "$dir/portal-desktop" "$bin/portal-desktop"
  install -m 644 "$dir/portal-desktop.png" "$data/icons/hicolor/256x256/apps/portal-desktop.png"
  # The menu entry runs this copy, whether or not $bin is on PATH.
  sed "s|^Exec=.*|Exec=$bin/portal-desktop|" "$dir/portal-desktop.desktop" \
    >"$data/applications/portal-desktop.desktop"
  chmod 644 "$data/applications/portal-desktop.desktop"
  if command -v update-desktop-database >/dev/null; then
    update-desktop-database -q "$data/applications" || true
  fi
  echo "Installed: $bin/portal-desktop"
  echo "Open Portal from your applications menu."
fi
