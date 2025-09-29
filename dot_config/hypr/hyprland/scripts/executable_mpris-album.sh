#!/usr/bin/env bash
set -euo pipefail

out="/tmp/lock-album.png"
url="$(playerctl metadata mpris:artUrl 2>/dev/null || true)"

# Nothing playing
[ -z "${url:-}" ] && { echo "$out"; exit 0; }

tmp="$(mktemp --suffix=.img)"
case "$url" in
  http*|https*)  curl -fsSL "$url" -o "$tmp" ;;
  file://*)      cp -f -- "${url#file://}" "$tmp" ;;
  *)             cp -f -- "$url" "$tmp" || true ;;
esac

# Ensure PNG (hyprlock image reload historically happier with png)
mime="$(file -b --mime-type "$tmp" 2>/dev/null || echo "")"
if echo "$mime" | grep -qiE 'jpeg|jpg'; then
  if command -v magick >/dev/null 2>&1; then
    magick "$tmp" PNG24:"$out"
  elif command -v convert >/dev/null 2>&1; then
    convert "$tmp" PNG24:"$out"
  elif command -v ffmpeg >/dev/null 2>&1; then
    ffmpeg -loglevel quiet -y -i "$tmp" "$out"
  else
    # fallback: just copy; may fail to reload if jpg
    cp -f -- "$tmp" "$out"
  fi
else
  cp -f -- "$tmp" "$out"
fi
rm -f -- "$tmp"
echo "$out"
