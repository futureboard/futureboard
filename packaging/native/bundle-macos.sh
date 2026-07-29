#!/usr/bin/env bash
# Bundle the xtask-staged Community Edition runtime into a macOS .app.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

PACKAGE_DIR="${1:-}"
OUT="${2:-$ROOT/packaging/native/out}"

if [[ -z "$PACKAGE_DIR" ]]; then
  PACKAGE_DIR="$(find "$ROOT/out/release/community" -mindepth 1 -maxdepth 1 -type d -name 'macos-*' -print -quit 2>/dev/null || true)"
fi

APP_NAME="Futureboard Studio"
APP_DIR="$OUT/$APP_NAME.app"

# IMPORTANT:
# This must match CFBundleExecutable in Info.plist.
# If Info.plist says futureboard_native, keep this.
# If Info.plist says FutureboardNative, change this to FutureboardNative.
APP_EXECUTABLE_NAME="FutureboardNative"
CEF_HELPER_BINARY_NAME="futureboard_cef_helper"
CEF_HELPER_BASE_NAME="$APP_NAME Helper"
CEF_HELPER_VARIANTS=(
  "Helper"
  "Helper (GPU)"
  "Helper (Renderer)"
  "Helper (Plugin)"
  "Helper (Alerts)"
)

ICON_SRC="$ROOT/packages/shared/app/icons/icon.icns"
PLIST_SRC="$ROOT/packaging/native/Info.plist"

CONTENTS="$APP_DIR/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"
FRAMEWORKS="$CONTENTS/Frameworks"

if [[ -z "$PACKAGE_DIR" || ! -f "$PACKAGE_DIR/FutureboardNative" ]]; then
  echo "error: xtask macOS runtime package not found: ${PACKAGE_DIR:-<none>}" >&2
  echo "run: cargo xtask package --profile release --edition community --plugin all" >&2
  exit 1
fi

if [[ ! -f "$PACKAGE_DIR/build-info.json" ]]; then
  echo "error: missing xtask package metadata: $PACKAGE_DIR/build-info.json" >&2
  exit 1
fi

if [[ ! -f "$PLIST_SRC" ]]; then
  echo "error: Info.plist not found: $PLIST_SRC" >&2
  exit 1
fi
if [[ ! -f "$PACKAGE_DIR/$CEF_HELPER_BINARY_NAME" ]]; then
  echo "error: staged macOS CEF helper not found: $PACKAGE_DIR/$CEF_HELPER_BINARY_NAME" >&2
  exit 1
fi

rm -rf "$APP_DIR"
mkdir -p "$MACOS" "$RESOURCES" "$FRAMEWORKS"

# Existing Info.plist
cp "$PLIST_SRC" "$CONTENTS/Info.plist"

# Preserve the validated xtask runtime layout, then place CEF in the standard
# macOS framework location expected by cef-rs' library loader.
cp -a "$PACKAGE_DIR/." "$MACOS/"
CEF_FRAMEWORK="$MACOS/Chromium Embedded Framework.framework"
if [[ ! -d "$CEF_FRAMEWORK" ]]; then
  echo "error: staged CEF framework not found: $CEF_FRAMEWORK" >&2
  exit 1
fi
mv "$CEF_FRAMEWORK" "$FRAMEWORKS/"
chmod +x "$MACOS/$APP_EXECUTABLE_NAME"
chmod +x "$MACOS/FutureboardPluginHostX64" "$MACOS/FutureboardPluginScanner"

# CEF 150's macOS sample packages five helper variants. They share the same
# minimal Rust executable but require distinct application/executable names so
# Chromium can select the appropriate role.
for VARIANT in "${CEF_HELPER_VARIANTS[@]}"; do
  HELPER_NAME="$APP_NAME $VARIANT"
  HELPER_APP="$FRAMEWORKS/$HELPER_NAME.app"
  HELPER_MACOS="$HELPER_APP/Contents/MacOS"
  mkdir -p "$HELPER_MACOS"
  cp "$MACOS/$CEF_HELPER_BINARY_NAME" "$HELPER_MACOS/$HELPER_NAME"
  chmod +x "$HELPER_MACOS/$HELPER_NAME"
  cat > "$HELPER_APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>$HELPER_NAME</string>
  <key>CFBundleIdentifier</key><string>org.futureboard.studio.native</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>$HELPER_NAME</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
  plutil -lint "$HELPER_APP/Contents/Info.plist" >/dev/null
done
rm "$MACOS/$CEF_HELPER_BINARY_NAME"

# Icon
if [[ -f "$ICON_SRC" ]]; then
  cp "$ICON_SRC" "$RESOURCES/icon.icns"
else
  echo "warning: missing $ICON_SRC — app will use default icon" >&2
fi

echo "Bundled macOS app: $APP_DIR"
echo
echo "Contents/MacOS:"
ls -la "$MACOS"
echo
echo "Contents/Frameworks:"
ls -la "$FRAMEWORKS"
echo
echo "Contents/Resources:"
ls -la "$RESOURCES"
