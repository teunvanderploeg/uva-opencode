"""Separate immutable packaged resources from private per-user login state."""
import os
import sys
from contextlib import contextmanager
from pathlib import Path

FROZEN = bool(getattr(sys, 'frozen', False))
ROOT = Path(getattr(sys, '_MEIPASS', Path(__file__).resolve().parent))
APP = Path(sys.executable).resolve().parent if FROZEN else ROOT
if FROZEN:
    if sys.platform == 'win32':
        STATE = Path(os.environ.get('LOCALAPPDATA', Path.home() / 'AppData/Local')) / 'UvAOpenCode'
    elif sys.platform == 'darwin':
        STATE = Path.home() / 'Library/Application Support/UvAOpenCode'
    else:
        STATE = Path(os.environ.get('XDG_DATA_HOME', Path.home() / '.local/share')) / 'uva-opencode'
else:
    STATE = ROOT / '.runtime'
BROWSERS = APP / 'browsers' if FROZEN else STATE / 'browsers'

@contextmanager
def file_lock(path):
    with path.open('a+b') as handle:
        if os.name == 'nt':
            import msvcrt
            if path.stat().st_size == 0:
                handle.write(b'0'); handle.flush()
            handle.seek(0)
            msvcrt.locking(handle.fileno(), msvcrt.LK_LOCK, 1)
        else:
            import fcntl
            fcntl.flock(handle, fcntl.LOCK_EX)
        try:
            yield
        finally:
            if os.name == 'nt':
                handle.seek(0)
                msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)
