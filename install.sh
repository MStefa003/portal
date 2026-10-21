#!/bin/sh
# Installs the latest Portal release on macOS or Linux.
#   curl -fsSL https://raw.githubusercontent.com/MStefa003/portal/main/install.sh | sh
set -eu

repo="MStefa003/portal"
dest="${PORTAL_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) echo "Unsupported CPU: $(uname -m)" >&2; exit 1 ;;
esac
case "$(uname -s)" in
  Darwin) target="$arch-apple-darwin" ;;
  Linux) target="$arch-unknown-linux-gnu" ;;
  *) echo "Unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac

url=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" \
  | grep -o "https://[^\"]*-$target\.tar\.gz" | head -n 1) || true
if [ -z "$url" ]; then
  echo "Could not find a Portal download for $target. See https://github.com/$repo/releases" >&2
  exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
file="$tmp/portal.tar.gz"
echo "Downloading $url"
curl -fsSL "$url" -o "$file"
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

tar xzf "$file" -C "$tmp"
mkdir -p "$dest"
install -m 755 "$tmp"/portal-*/portal "$tmp"/portal-*/portal-rendezvous "$dest"
echo "Installed: $dest/portal"
case ":$PATH:" in
  *":$dest:"*) ;;
  *) echo "Add $dest to your PATH to run 'portal' from anywhere." ;;
esac
