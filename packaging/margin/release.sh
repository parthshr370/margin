#!/usr/bin/env bash
# Margin's release, built on this machine (Margin has no hosted CI):
#
#   1. packaging/margin/check.sh (skip with --skip-checks)
#   2. the x86_64 Linux packages (AppImage + .zsync, .deb, .rpm, .tar.gz) for glibc 2.35 and newer
#      (Ubuntu 22.04, Debian 12, Fedora 36 and later), linked with cargo-zigbuild, with the
#      translations and Japanese face upstream's releases embed (craft-i18n, craft-fonts)
#   3. checks of the packages, and SHA256SUMS
#   4. the DRAFT GitHub Release v<version> on parthshr370/margin: created at HEAD, or its files
#      replaced if the draft exists. Publishing stays a manual step. --no-upload stops before this.
#
# Usage: packaging/margin/release.sh [--skip-checks] [--no-upload] [--notes FILE]
#
# <version> is `version` in apps/margin/Cargo.toml. HEAD must be committed and pushed. Needs zig,
# cargo-zigbuild (`cargo install --locked cargo-zigbuild`) and a logged-in gh; nfpm and
# appimagetool are downloaded into the target directory when missing. No aarch64 or macOS builds.
set -euo pipefail
# shellcheck source=env.sh
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"

CHECKS=1
UPLOAD=1
NOTES=""
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-checks) CHECKS=0; shift ;;
    --no-upload) UPLOAD=0; shift ;;
    --notes) NOTES="$2"; shift 2 ;;
    -h | --help) sed -n '2,17p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

REPO=parthshr370/margin
GLIBC=2.35
TAG="v$VERSION"
export MARGIN_TARGET="x86_64-unknown-linux-gnu.$GLIBC"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-$(( ($(nproc) + 1) / 2 ))}"
NFPM_VERSION=2.49.0

[ "$(uname -m)" = x86_64 ] || { echo "error: release.sh builds x86_64 packages on an x86_64 machine" >&2; exit 2; }
command -v zig >/dev/null || { echo "error: zig not found (see the header of $0)" >&2; exit 1; }
# A cargo subcommand: cargo finds it in ~/.cargo/bin even when that isn't on PATH.
cargo zigbuild --help >/dev/null 2>&1 || { echo "error: cargo-zigbuild not found (see the header of $0)" >&2; exit 1; }
if [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
  echo "error: the working tree has changes; commit them first so the release matches a commit" >&2
  exit 1
fi
SHA="$(git -C "$ROOT" rev-parse HEAD)"
if [ "$UPLOAD" = 1 ]; then
  gh api "repos/$REPO/commits/$SHA" --silent 2>/dev/null ||
    { echo "error: $SHA is not on github.com/$REPO; push it first" >&2; exit 1; }
fi

echo "==> Margin $VERSION release from ${SHA::9}"
if [ "$CHECKS" = 1 ]; then "$ROOT/packaging/margin/check.sh"; fi

if ! command -v nfpm >/dev/null; then
  tools="$CARGO_TARGET_DIR/margin-tools"
  if [ ! -x "$tools/nfpm" ]; then
    mkdir -p "$tools"
    curl -fsSL "https://github.com/goreleaser/nfpm/releases/download/v$NFPM_VERSION/nfpm_${NFPM_VERSION}_Linux_x86_64.tar.gz" |
      tar -xz -C "$tools" nfpm
  fi
  export PATH="$tools:$PATH"
fi

# Only this script's own files, in case DIST points somewhere shared.
rm -f "$DIST"/margin-* "$DIST"/SHA256SUMS
mkdir -p "$DIST"
"$ROOT/packaging/margin/linux/package.sh"

echo "==> checking the packages"
BASENAME="margin-$VERSION-linux-x86_64"
STAGED="$CARGO_TARGET_DIR/margin-linux-package/root/usr/bin/margin"
newest="$(objdump -T "$STAGED" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
if [ "$(printf '%s\n%s\n' "$newest" "GLIBC_$GLIBC" | sort -V | tail -1)" != "GLIBC_$GLIBC" ]; then
  echo "error: the binary needs $newest, newer than glibc $GLIBC" >&2
  exit 1
fi
APPIMAGE="$DIST/$BASENAME.AppImage"
APPIMAGE_EXTRACT_AND_RUN=1 "$APPIMAGE" --version | grep -Fx "margin $VERSION"
"$APPIMAGE" --appimage-updateinformation | grep -F 'gh-releases-zsync|' >/dev/null
test -s "$APPIMAGE.zsync"
# Every format is built from the same staged tree; the tarball shows what it holds.
listing="$(tar -tzf "$DIST/$BASENAME.tar.gz")"
for file in margin/models/text-detection.onnx margin/models/text-detection.onnx.LICENCE.txt \
  margin/models/text-recognition.onnx margin/models/text-recognition.onnx.LICENCE.txt \
  doc/margin/NOTICE doc/margin/ATTRIBUTION.md; do
  grep -Eq "/share/$file\$" <<<"$listing" || { echo "error: the tarball lacks share/$file" >&2; exit 1; }
done
for fmt in deb rpm; do test -s "$DIST/$BASENAME.$fmt"; done
(cd "$DIST" && sha256sum -- margin-"$VERSION"-* >SHA256SUMS && cat SHA256SUMS)

if [ "$UPLOAD" = 0 ]; then
  echo "==> packages in $DIST (not uploaded)"
  exit 0
fi

echo "==> draft release $TAG"
if gh release view "$TAG" -R "$REPO" --json isDraft -q .isDraft >/dev/null 2>&1; then
  if [ "$(gh release view "$TAG" -R "$REPO" --json isDraft -q .isDraft)" != true ]; then
    echo "error: $TAG is already published; bump the version in apps/margin/Cargo.toml" >&2
    exit 1
  fi
  gh release edit "$TAG" -R "$REPO" --target "$SHA" ${NOTES:+--notes-file "$NOTES"}
  # Files of an earlier build of this draft that this build doesn't make go too.
  for old in $(gh release view "$TAG" -R "$REPO" --json assets -q '.assets[].name'); do
    [ -e "$DIST/$old" ] || gh release delete-asset "$TAG" "$old" -R "$REPO" --yes
  done
  gh release upload "$TAG" -R "$REPO" --clobber "$DIST"/margin-"$VERSION"-* "$DIST/SHA256SUMS"
else
  default_notes="$CARGO_TARGET_DIR/margin-release-notes.md"
  cat >"$default_notes" <<EOF
Margin $VERSION, a calm, reader-first PDF app built on the PdfCraft engine.

## Downloads

- Linux x86_64: \`.AppImage\` (any distro), \`.deb\` (Debian, Ubuntu), \`.rpm\` (Fedora, openSUSE), \`.tar.gz\` (plain tree). Needs glibc $GLIBC or newer (Ubuntu 22.04, Debian 12, Fedora 36 and later).

Check downloads against \`SHA256SUMS\`.
EOF
  gh release create "$TAG" -R "$REPO" --draft --target "$SHA" --title "Margin $TAG" \
    --notes-file "${NOTES:-$default_notes}" "$DIST"/margin-"$VERSION"-* "$DIST/SHA256SUMS"
fi
gh release view "$TAG" -R "$REPO" --json url,isDraft -q '.url + " (draft: " + (.isDraft|tostring) + ")"'
