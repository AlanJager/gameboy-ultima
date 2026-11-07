#!/bin/sh
set -eu

ROOT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
APP_PATH="$ROOT_DIR/target/ULTIMA.app"
CONTENTS_PATH="$APP_PATH/Contents"

cargo build --manifest-path "$ROOT_DIR/Cargo.toml" --release --bin gameboy-ultima-gpui
mkdir -p "$CONTENTS_PATH/MacOS"
cp "$ROOT_DIR/target/release/gameboy-ultima-gpui" "$CONTENTS_PATH/MacOS/gameboy-ultima-gpui"
cp "$ROOT_DIR/macos/Info.plist" "$CONTENTS_PATH/Info.plist"
codesign --force --deep --sign - "$APP_PATH"

printf 'Packaged app: %s\n' "$APP_PATH"
printf 'Open it from Finder or Spotlight to launch without a Terminal window.\n'
