#!/bin/sh
set -eu

pnpm tauri build --bundles app

APP="src-tauri/target/release/bundle/macos/EasyCopy.app"
VERSION="$(node -p 'JSON.parse(require("fs").readFileSync("package.json", "utf8")).version')"
DMG="docs/downloads/EasyCopy-${VERSION}-macOS-arm64.dmg"
codesign --force --deep --sign - \
  --identifier com.blue.easycopy \
  --requirements '=designated => identifier "com.blue.easycopy"' \
  "$APP"

hdiutil create -volname "EasyCopy ${VERSION}" -srcfolder "$APP" \
  -format UDZO -ov "$DMG"

echo "Built $APP and $DMG with stable bundle identifier com.blue.easycopy"
