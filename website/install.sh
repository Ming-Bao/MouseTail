#!/bin/sh
# Kiore installer for Linux:  curl -fsSL https://kiore.galen.green/install.sh | sh
# Downloads the latest release for this computer and installs it for the current user.
set -eu
repo=galengreen/kiore
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) echo "Kiore doesn't have a build for $(uname -m) yet."; exit 1 ;;
esac
url="https://github.com/$repo/releases/latest/download/kiore-linux-$arch.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "Downloading Kiore for Linux ($arch)…"
curl -fsSL "$url" | tar -xz -C "$tmp"
bash "$tmp/kiore-linux-$arch/install.sh"
