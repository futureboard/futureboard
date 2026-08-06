#!/usr/bin/env bash
# Create a compressed macOS DMG from Futureboard Studio.app.
#
# Usage: bundle-macos-dmg.sh [APP_DIR] [OUT_DIR] [APP_VERSION]
#   APP_DIR     Path to the .app bundle.
#               Default: packaging/native/out/Futureboard Studio.app
#   OUT_DIR     Directory the finished .dmg is written to.
#               Default: packaging/native/out
#   APP_VERSION Version string embedded in the DMG filename.
#               Default: read from version.json
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

APP_NAME="Futureboard Studio"
APP_DIR="${1:-$ROOT/packaging/native/out/$APP_NAME.app}"
OUT_DIR="${2:-$ROOT/packaging/native/out}"
APP_VERSION="${3:-}"

if [[ -z "$APP_VERSION" ]]; then
  APP_VERSION="$(grep -oE '"version"[[:space:]]*:[[:space:]]*"[^"]+"' "$ROOT/version.json" | sed -E 's/.*"([^"]+)"/\1/' || true)"
fi
if [[ -z "$APP_VERSION" ]]; then
  echo "error: could not determine app version (pass it as \$3, or ensure version.json is readable)" >&2
  exit 1
fi

if [[ ! -d "$APP_DIR" ]]; then
  echo "error: app bundle not found: $APP_DIR (run packaging/native/bundle-macos.sh first)" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"

# Name the image after what it actually contains. The in-app updater matches on
# `macos` + `.dmg` (apps/native/studio/src/updater.rs::platform_asset), so every
# suffix below stays resolvable.
APP_EXECUTABLE="$APP_DIR/Contents/MacOS/FutureboardNative"
ARCH_SUFFIX="macos"
if [[ -f "$APP_EXECUTABLE" ]]; then
  ARCHS="$(lipo -archs "$APP_EXECUTABLE" 2>/dev/null || true)"
  case "$ARCHS" in
    *x86_64*arm64* | *arm64*x86_64*) ARCH_SUFFIX="macos-universal" ;;
    *arm64*) ARCH_SUFFIX="macos-arm64" ;;
    *x86_64*) ARCH_SUFFIX="macos-x86_64" ;;
  esac
fi

DMG="$OUT_DIR/Futureboard.Studio-${APP_VERSION}-${ARCH_SUFFIX}.dmg"
TMP_DMG="$OUT_DIR/Futureboard.Studio-${APP_VERSION}-${ARCH_SUFFIX}.tmp.dmg"
STAGING_DIR="$(mktemp -d "${TMPDIR:-/tmp}/futureboard-dmg.XXXXXX")"
trap 'rm -rf "$STAGING_DIR" "$TMP_DMG"' EXIT

cp -R "$APP_DIR" "$STAGING_DIR/"
ln -s /Applications "$STAGING_DIR/Applications"

rm -f "$DMG" "$TMP_DMG"

hdiutil create \
  -volname "$APP_NAME" \
  -srcfolder "$STAGING_DIR" \
  -fs HFS+ \
  -format UDRW \
  "$TMP_DMG"

hdiutil convert "$TMP_DMG" \
  -format UDZO \
  -imagekey zlib-level=9 \
  -o "$DMG"

rm -f "$TMP_DMG"

echo "Built DMG: $DMG"
ls -lh "$DMG"
