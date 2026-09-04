#!/usr/bin/env sh
set -eu

missing=0
for tool in ffmpeg ffprobe exiftool; do
  if command -v "$tool" >/dev/null 2>&1; then
    printf 'OK      %s: %s\n' "$tool" "$(command -v "$tool")"
  else
    printf 'MISSING %s\n' "$tool"
    missing=1
  fi
done

if [ "$missing" -ne 0 ]; then
  printf '\nInstall the missing runtime tools, then run this check again.\n'
  case "$(uname -s)" in
    Darwin) printf 'macOS: brew install ffmpeg exiftool\n' ;;
    Linux) printf 'Debian/Ubuntu: sudo apt-get install ffmpeg libimage-exiftool-perl\n' ;;
  esac
  exit 1
fi

printf 'All mergeme runtime tools are available.\n'
