#!/bin/zsh
# Double-click in Finder to start OpenCode in your home folder.
SCRIPT_DIR=${0:A:h}
exec /usr/bin/env python3 "$SCRIPT_DIR/launch.py" "$HOME"
