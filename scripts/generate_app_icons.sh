#!/bin/sh
# Regenerates the app icons from crates/pistolame-app/assets/app-icon/pistolame.svg.
# Needs rsvg-convert and ImageMagick (`magick`); the .icns step needs macOS (`iconutil`).
# The generated files are committed, so the CI never needs these tools.
set -eu
cd "$(dirname "$0")/../crates/pistolame-app/assets/app-icon"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for size in 16 24 32 48 64 128 256 512 1024; do
  rsvg-convert -w "$size" -h "$size" pistolame.svg -o "$work/$size.png"
done
cp "$work/256.png" pistolame-256.png
cp "$work/512.png" pistolame-512.png
magick "$work/16.png" "$work/24.png" "$work/32.png" "$work/48.png" "$work/64.png" \
  "$work/128.png" "$work/256.png" pistolame.ico
iconset="$work/Pistolame.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
  cp "$work/$size.png" "$iconset/icon_${size}x${size}.png"
  cp "$work/$((size * 2)).png" "$iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$iconset" -o Pistolame.icns
echo "App icons regenerated."
