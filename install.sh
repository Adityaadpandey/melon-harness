#!/bin/sh
# Downloads and verifies the latest melon release for this machine into ~/.local/bin.
#   curl -fsSL https://raw.githubusercontent.com/Adityaadpandey/melon-harness/main/install.sh | sh
set -e
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)              t=aarch64-apple-darwin ;;
  Darwin-x86_64)             t=x86_64-apple-darwin ;;
  Linux-x86_64)              t=x86_64-unknown-linux-musl ;;
  Linux-aarch64|Linux-arm64) t=aarch64-unknown-linux-musl ;;
  *) echo "no prebuilt melon for $(uname -s)-$(uname -m); build from source with cargo" >&2; exit 1 ;;
esac
dir="${MELON_BIN:-$HOME/.local/bin}"
mkdir -p "$dir"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
base="https://github.com/Adityaadpandey/melon-harness/releases/latest/download/melon-$t.tar.gz"
curl -fsSL "$base" -o "$tmp/m.tar.gz"
curl -fsSL "$base.sha256" -o "$tmp/m.sha256"
want="$(cut -d' ' -f1 "$tmp/m.sha256")"
got="$( (sha256sum "$tmp/m.tar.gz" 2>/dev/null || shasum -a 256 "$tmp/m.tar.gz") | cut -d' ' -f1)"
[ "$want" = "$got" ] || { echo "checksum mismatch, not installing" >&2; exit 1; }
tar -xz -C "$dir" -f "$tmp/m.tar.gz" melon
echo "installed $dir/melon (make sure $dir is on your PATH)"
