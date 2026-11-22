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

# Absolute: nfpm rejects a source path with `..` in it (CARGO_TARGET_DIR=../target).
CARGO_TARGET_DIR="$(realpath -m "$CARGO_TARGET_DIR")"
export CARGO_TARGET_DIR

export MARGIN_APP_ID=io.github.parthshr370.Margin
export MARGIN_ASSETS="$ROOT/apps/margin/assets"

# The build inputs upstream's releases embed, at upstream's pins: craft-i18n (the 34 interface
# languages besides English, pinned in craft-i18n.rev) and craft-fonts (the Japanese interface
# face, pinned as craft-fonts-ref in .github/workflows/release.yml). Without them a build is
# English only. Each is a shallow checkout of the pinned commit in $CARGO_TARGET_DIR/margin-inputs,
# fetched once per pin; exports CRAFT_I18N_DIR and CRAFT_FONTS_DIR.
margin_build_inputs() {
  local i18n_rev fonts_rev
  i18n_rev="$(tr -d '[:space:]' <"$ROOT/craft-i18n.rev")"
  fonts_rev="$(awk '/craft-fonts-ref:/ { print $2; exit }' "$ROOT/.github/workflows/release.yml")"
  if [ -z "$i18n_rev" ] || [ -z "$fonts_rev" ]; then
    echo "error: could not read the craft-i18n or craft-fonts pin" >&2
    exit 1
  fi
  CRAFT_I18N_DIR="$(margin_pinned_checkout craft-i18n "$i18n_rev")"
  CRAFT_FONTS_DIR="$(margin_pinned_checkout craft-fonts "$fonts_rev")"
  export CRAFT_I18N_DIR CRAFT_FONTS_DIR
}

# Print the path of a shallow checkout of storytold/<repo> at <commit>, fetching it if needed.
margin_pinned_checkout() {
  local repo="$1" rev="$2" dir="$CARGO_TARGET_DIR/margin-inputs/$1"
  if [ "$(git -C "$dir" rev-parse HEAD 2>/dev/null)" != "$rev" ]; then
    rm -rf "$dir"
    mkdir -p "$dir"
    git -C "$dir" init -q
    git -C "$dir" fetch -q --depth 1 "https://github.com/storytold/$repo" "$rev" >&2
    git -C "$dir" checkout -q FETCH_HEAD
  fi
  printf '%s\n' "$dir"
}

# packaging/env.sh's licence and readme files, NOTICE (the PdfCraft attribution) and
# ATTRIBUTION.md, which credits the bundled OCR models (required: their licence asks for it).
copy_margin_docs() {
  copy_docs "$1"
  if [ -f "$ROOT/NOTICE" ]; then cp "$ROOT/NOTICE" "$1/"; fi
  cp "$ROOT/ATTRIBUTION.md" "$1/"
}
