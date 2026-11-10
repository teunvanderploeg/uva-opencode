#!/bin/sh
set -eu
SOURCE_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
TARGET_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/uva-opencode-app"
mkdir -p "$TARGET_DIR" "$HOME/.local/bin"
cp -R "$SOURCE_DIR/app/." "$TARGET_DIR/"
chmod +x "$TARGET_DIR/uva-opencode" "$TARGET_DIR/vendor/opencode"
if [ -e "$HOME/.local/bin/uva-opencode" ] && [ ! -L "$HOME/.local/bin/uva-opencode" ]; then
  echo 'Existing uva-opencode command is not a symlink; left unchanged.' >&2
  exit 1
fi
ln -sfn "$TARGET_DIR/uva-opencode" "$HOME/.local/bin/uva-opencode"
echo 'Installed. Open a terminal in your project and run uva-opencode.'
echo 'Ensure ~/.local/bin is on PATH. Linux desktop browser libraries must be available.'
