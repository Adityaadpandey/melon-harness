#!/bin/sh
# Downloads the latest melon release for this machine into ~/.local/bin.
#   curl -fsSL https://raw.githubusercontent.com/Adityaadpandey/melon-harness/main/install.sh | sh
set -e
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)              t=aarch64-apple-darwin ;;
  Darwin-x86_64)             t=x86_64-apple-darwin ;;
  Linux-x86_64)              t=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64) t=aarch64-unknown-linux-gnu ;;
  *) echo "no prebuilt melon for $(uname -s)-$(uname -m); build from source with cargo" >&2; exit 1 ;;
esac
dir="${MELON_BIN:-$HOME/.local/bin}"
mkdir -p "$dir"
curl -fsSL "https://github.com/Adityaadpandey/melon-harness/releases/latest/download/melon-$t.tar.gz" | tar -xz -C "$dir" melon
echo "installed $dir/melon (make sure $dir is on your PATH)"
