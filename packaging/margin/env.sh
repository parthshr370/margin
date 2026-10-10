# shellcheck shell=bash
# Shared setup for Margin's packaging scripts. Source it: `. "$(dirname "$0")/../env.sh"`.
#
# Margin has its own version (`version` in apps/margin/Cargo.toml, override: MARGIN_VERSION),
# not the PdfCraft workspace version. Everything else comes from packaging/env.sh (ROOT, DIST,
# CARGO_TARGET_DIR, the build commit and date, and its helpers).

MARGIN_MANIFEST="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/apps/margin/Cargo.toml"

# The first `version = "…"` in the manifest, which is the [package] one.
margin_version() {
  awk -F'"' '/^version[[:space:]]*=/ { print $2; exit }' "$MARGIN_MANIFEST"
}

PDFCRAFT_VERSION="${MARGIN_VERSION:-$(margin_version)}"
if [ -z "$PDFCRAFT_VERSION" ]; then
  echo "error: could not read the version from $MARGIN_MANIFEST" >&2
  exit 1
fi
export PDFCRAFT_VERSION
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"

export MARGIN_APP_ID=io.github.parthshr370.Margin
export MARGIN_ASSETS="$ROOT/apps/margin/assets"

# packaging/env.sh's licence and readme files plus NOTICE (the PdfCraft attribution).
copy_margin_docs() {
  copy_docs "$1"
  if [ -f "$ROOT/NOTICE" ]; then cp "$ROOT/NOTICE" "$1/"; fi
}
