#!/usr/bin/env bash
# Regenerate Margin's packaging icons from apps/margin/assets/margin.svg (the master vector):
#
#   apps/margin/assets/hicolor/<s>x<s>/apps/io.github.parthshr370.Margin.png  Linux, s = 48 128 256 512
#   apps/margin/assets/hicolor/scalable/apps/io.github.parthshr370.Margin.svg
#   apps/margin/assets/margin-macos-1024.png  Apple's icon grid; packaging/margin/macos/package.sh
#                                             turns it into Margin.icns
#
# Needs: rsvg-convert (librsvg) or resvg, and ImageMagick (magick) for the macOS padding.
# The outputs are committed, so packaging never needs these tools. After running it, update the
# sha256 values in ATTRIBUTION.toml, then `cargo xtask assets --write`.
#
#   packaging/margin/icons.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIR="$ROOT/apps/margin/assets"
SVG="$DIR/margin.svg"
ID="io.github.parthshr370.Margin"
# Apple's grid: an 824 px body centred on a transparent 1024 canvas. The master is a full-bleed
# 512 tile, right for Linux as is.
MAC_CANVAS=1024
MAC_BODY=824

render() {
  if command -v resvg >/dev/null; then
    resvg -w "$2" -h "$2" "$1" "$3" </dev/null
  elif command -v rsvg-convert >/dev/null; then
    rsvg-convert -w "$2" -h "$2" -o "$3" "$1"
  else
    echo "error: neither resvg nor rsvg-convert found" >&2
    exit 1
  fi
}
command -v magick >/dev/null || { echo "error: magick (ImageMagick) not found" >&2; exit 1; }

for s in 48 128 256 512; do
  mkdir -p "$DIR/hicolor/${s}x${s}/apps"
  render "$SVG" "$s" "$DIR/hicolor/${s}x${s}/apps/$ID.png"
done
mkdir -p "$DIR/hicolor/scalable/apps"
cp "$SVG" "$DIR/hicolor/scalable/apps/$ID.svg"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
render "$SVG" "$MAC_BODY" "$TMP/body.png"
magick "$TMP/body.png" -background none -gravity center -extent "${MAC_CANVAS}x${MAC_CANVAS}" \
  -strip "PNG32:$DIR/margin-macos-1024.png"
echo "icons written to $DIR"
