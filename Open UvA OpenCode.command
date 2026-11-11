#!/bin/zsh
# Double-click in Finder to start OpenCode in your home folder.
SCRIPT_DIR=${0:A:h}
BINARY="$SCRIPT_DIR/dist/native/uva-opencode/uva-opencode"
if [[ -x "$BINARY" ]]; then
  exec "$BINARY" "$HOME"
fi
exec /usr/bin/env cargo run --manifest-path "$SCRIPT_DIR/Cargo.toml" -- "$HOME"
