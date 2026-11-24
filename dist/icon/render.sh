#!/bin/sh
# Regenerates the icon files from the SVGs. Needs rsvg-convert, ImageMagick and
# png2icns:
#   docker run --rm -v "$PWD":/work -w /work debian:bookworm-slim sh -c \
#     'apt-get update -qq && apt-get install -y -qq librsvg2-bin imagemagick icnsutils && dist/icon/render.sh'
set -eu
cd "$(dirname "$0")"
ASSETS=../../crates/gui/assets
mkdir -p "$ASSETS"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# Raw RGBA, so the GUI needs no image decoder: the window icon, and the tray
# glyph whose alpha the tray fills with the state color.
rsvg-convert -w 64 -h 64 farplug.svg -o "$TMP/app64.png"
convert "$TMP/app64.png" -depth 8 "rgba:$ASSETS/icon-64.rgba"
rsvg-convert -w 32 -h 32 farplug-glyph.svg -o "$TMP/glyph32.png"
convert "$TMP/glyph32.png" -depth 8 "rgba:$ASSETS/glyph-32.rgba"

for s in 16 24 32 48 64 256; do
    rsvg-convert -w $s -h $s farplug.svg -o "$TMP/app$s.png"
done
convert "$TMP"/app16.png "$TMP"/app24.png "$TMP"/app32.png "$TMP"/app48.png \
    "$TMP"/app64.png "$TMP"/app256.png farplug.ico

# The macOS app bundle's icon.
for s in 128 512 1024; do
    rsvg-convert -w $s -h $s farplug.svg -o "$TMP/app$s.png"
done
png2icns farplug.icns "$TMP"/app16.png "$TMP"/app32.png "$TMP"/app128.png \
    "$TMP"/app256.png "$TMP"/app512.png "$TMP"/app1024.png >/dev/null
