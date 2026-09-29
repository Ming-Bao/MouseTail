#!/bin/sh
# MouseTail installer for Linux:  curl -fsSL https://mousetail.galen.green/install.sh | sh
# Downloads the latest release for this computer and installs it for the current user.
set -eu
repo=galengreen/mousetail
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) echo "MouseTail doesn't have a build for $(uname -m) yet."; exit 1 ;;
esac
url="https://github.com/$repo/releases/latest/download/mousetail-linux-$arch.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "Downloading MouseTail for Linux ($arch)…"
curl -fsSL "$url" | tar -xz -C "$tmp"
bash "$tmp/mousetail-linux-$arch/install.sh"
