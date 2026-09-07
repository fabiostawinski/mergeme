#!/usr/bin/env sh
set -eu

repo="${MERGEME_REPO:-fabiostawinski/mergeme}"
version="${MERGEME_VERSION:-latest}"
prefix="${MERGEME_PREFIX:-$HOME/.local/bin}"
mkdir -p "$prefix"

os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
  Darwin/arm64) target="aarch64-apple-darwin" ;;
  Darwin/x86_64) target="x86_64-apple-darwin" ;;
  Linux/x86_64) target="x86_64-unknown-linux-gnu" ;;
  *) printf 'Unsupported platform: %s/%s\n' "$os" "$arch" >&2; exit 1 ;;
esac

if [ "$version" = latest ]; then
  url="https://github.com/$repo/releases/latest/download/mergeme-$target.tar.gz"
else
  case "$version" in
    mergeme-v*) tag="$version" ;;
    v*) tag="mergeme-$version" ;;
    *) tag="mergeme-v$version" ;;
  esac
  url="https://github.com/$repo/releases/download/$tag/mergeme-$target.tar.gz"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl --fail --location --silent --show-error "$url" | tar -xz -C "$tmp"
install -m 755 "$tmp/mergeme-$target/mergeme" "$prefix/mergeme"
printf 'Installed mergeme to %s/mergeme\n' "$prefix"

if ! command -v ffmpeg >/dev/null 2>&1 || ! command -v ffprobe >/dev/null 2>&1 || ! command -v exiftool >/dev/null 2>&1; then
  printf '\nRuntime tools are still missing. Run: %s\n' "$(dirname "$0")/check-tools.sh"
else
  printf 'Runtime tools detected.\n'
fi
