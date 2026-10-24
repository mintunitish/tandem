#!/usr/bin/env bash
# Build Tandem.app (cargo-bundle), ad-hoc codesign it, and package Tandem-<ver>-<arch>.dmg.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target/app}"
command -v cargo-bundle >/dev/null || { echo "install cargo-bundle: cargo install cargo-bundle" >&2; exit 1; }

VERSION="$(cargo metadata --no-deps --format-version 1 | python3 -c \
  'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="tandem-app"))')"
ARCH="$(uname -m)"
SIGN_ID="${CODESIGN_IDENTITY:--}"   # "-" = ad-hoc; set to "Developer ID Application: ..." for real signing

cargo bundle --release -p tandem-app
rm -rf "$CARGO_TARGET_DIR/release/incremental"

APP="$CARGO_TARGET_DIR/release/bundle/osx/Tandem.app"
[ -d "$APP" ] || { echo "missing $APP" >&2; exit 1; }

if [ "$SIGN_ID" = "-" ]; then
  codesign --force --deep --sign - "$APP"
else
  codesign --force --deep --options runtime --timestamp --sign "$SIGN_ID" "$APP"
fi
codesign --verify --deep --strict "$APP"

DMG="$CARGO_TARGET_DIR/release/bundle/Tandem-$VERSION-$ARCH.dmg"
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/tandem-dmg.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname Tandem -srcfolder "$STAGE" -ov -format UDZO "$DMG"

echo "App: $APP"
echo "DMG: $DMG ($(du -h "$DMG" | cut -f1))"
