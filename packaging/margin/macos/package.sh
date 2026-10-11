#!/usr/bin/env bash
# Build and sign Margin for macOS:
#
#   $DIST/margin-<version>-macos-<arch>.dmg   Margin.app on a drag-to-Applications DMG
#
# Usage: packaging/margin/macos/package.sh [--arch universal|aarch64|x86_64] [--skip-build]
#
# The Margin twin of packaging/macos/package.sh: it builds only the `margin` crate, makes
# Margin.icns from apps/margin/assets/margin-macos-1024.png (sips and iconutil, both part of
# macOS) and signs ad-hoc by default. Margin is not notarized, so on another Mac Gatekeeper
# asks the user to allow it once (right-click > Open, or System Settings > Privacy & Security).
#
# Signing (env):
#   MACOS_SIGN_IDENTITY   codesign identity (name or SHA-1). Default "-" = ad-hoc.
#   MACOS_KEYCHAIN        keychain holding the identity (optional)
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/margin/macos"

ARCH=universal
SKIP_BUILD=0
while [ $# -gt 0 ]; do
  case "$1" in
    --arch) ARCH="$2"; shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    -h | --help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$ARCH" in
  universal) TARGETS=(aarch64-apple-darwin x86_64-apple-darwin) ;;
  aarch64) TARGETS=(aarch64-apple-darwin) ;;
  x86_64) TARGETS=(x86_64-apple-darwin) ;;
  *) echo "unknown --arch $ARCH" >&2; exit 2 ;;
esac

# Keep in sync with LSMinimumSystemVersion in Info.plist.in.
export MACOSX_DEPLOYMENT_TARGET=11.0
IDENTITY="${MACOS_SIGN_IDENTITY:--}"
SHORT_VERSION="${VERSION%%-*}"
WORK="$CARGO_TARGET_DIR/margin-macos-package"
APP="$WORK/Margin.app"
DMG="$DIST/margin-$VERSION-macos-$ARCH.dmg"

if [ "$IDENTITY" = "-" ]; then
  warn "macOS: ad-hoc signing (no MACOS_SIGN_IDENTITY); Margin is not notarized"
fi
echo "==> Margin $VERSION for macOS ($ARCH), identity: $IDENTITY"

# ---- build -------------------------------------------------------------------------------------
if [ "$SKIP_BUILD" = 0 ]; then
  args=()
  for t in "${TARGETS[@]}"; do args+=(--target "$t"); done
  (cd "$ROOT" && cargo build --release --locked -p margin "${args[@]}")
fi

rm -rf "$WORK"
mkdir -p "$WORK/bin"
inputs=()
for t in "${TARGETS[@]}"; do inputs+=("$CARGO_TARGET_DIR/$t/release/margin"); done
lipo -create -output "$WORK/bin/margin" "${inputs[@]}"
lipo -info "$WORK/bin/margin"

sign() {
  # Hardened runtime + secure timestamp for a real identity; ad-hoc can't be timestamped.
  local ts=(--timestamp)
  [ "$IDENTITY" = "-" ] && ts=(--timestamp=none)
  local kc=()
  [ -n "${MACOS_KEYCHAIN:-}" ] && kc=(--keychain "$MACOS_KEYCHAIN")
  codesign --force --sign "$IDENTITY" ${kc[@]+"${kc[@]}"} "${ts[@]}" "$@"
}

# ---- Margin.icns -------------------------------------------------------------------------------
# Every size iconutil expects, scaled down from the 1024 px master on Apple's grid.
ICONSET="$WORK/Margin.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s" "$MARGIN_ASSETS/margin-macos-1024.png" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) "$MARGIN_ASSETS/margin-macos-1024.png" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns -o "$WORK/Margin.icns" "$ICONSET"

# ---- Margin.app --------------------------------------------------------------------------------
echo "==> assembling $APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
# Executable and icon carry the display name (CFBundleExecutable / CFBundleIconFile).
cp "$WORK/bin/margin" "$APP/Contents/MacOS/Margin"
cp "$WORK/Margin.icns" "$APP/Contents/Resources/Margin.icns"
copy_margin_docs "$APP/Contents/Resources"
copy_margin_models "$APP/Contents/Resources/models"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@SHORT_VERSION@/$SHORT_VERSION/g" \
  -e "s/@BUILD_SHA@/${PDFCRAFT_BUILD_SHA:-unknown}/g" \
  "$HERE/Info.plist.in" >"$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
printf 'APPL????' >"$APP/Contents/PkgInfo"

# Inside-out: the executable, then the bundle (no --deep on the final signature). PdfCraft's
# hardened-runtime entitlements (none) fit Margin too.
sign --options runtime --entitlements "$ROOT/packaging/macos/entitlements.plist" "$APP/Contents/MacOS/Margin"
sign --options runtime --entitlements "$ROOT/packaging/macos/entitlements.plist" "$APP"
codesign --verify --strict --deep --verbose=2 "$APP"

# ---- DMG ---------------------------------------------------------------------------------------
echo "==> building $DMG"
STAGE="$WORK/dmg"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/Margin.app"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG" "$WORK/raw.dmg"
# makehybrid + convert builds the image without attaching a device, unlike `create -srcfolder`,
# which is flaky on CI runners ("Resource busy").
hdiutil makehybrid -hfs -hfs-volume-name "Margin" -hfs-openfolder "$STAGE" -o "$WORK/raw.dmg" "$STAGE"
hdiutil convert "$WORK/raw.dmg" -format UDZO -imagekey zlib-level=9 -o "$DMG"
rm -f "$WORK/raw.dmg"
sign "$DMG"
codesign --verify --strict --verbose=2 "$DMG"

"$APP/Contents/MacOS/Margin" --version
echo "==> done"
ls -lh "$DMG"
