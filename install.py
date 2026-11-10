#!/usr/bin/env python3
"""One-time local installation. Run from a cloned/downloaded source folder."""
import os
from pathlib import Path
import subprocess
import sys
import venv

ROOT = Path(__file__).resolve().parent

def main():
    if os.name == 'nt':
        raise SystemExit('This installer currently supports macOS and Linux. Windows packaging is pending.')
    directory = ROOT / '.venv'
    if not (directory / 'bin/python').exists():
        venv.EnvBuilder(with_pip=True).create(directory)
    python = directory / 'bin/python'
    subprocess.run([str(python), '-m', 'pip', 'install', '-r', str(ROOT / 'requirements.txt')], check=True)
    environment = os.environ.copy()
    environment['PLAYWRIGHT_BROWSERS_PATH'] = str(ROOT / '.runtime/browsers')
    subprocess.run([str(python), '-m', 'playwright', 'install', 'chromium'], env=environment, check=True)
    binary = Path.home() / '.local/bin/uva-opencode'
    binary.parent.mkdir(parents=True, exist_ok=True)
    source = ROOT / 'launch.py'
    source.chmod(source.stat().st_mode | 0o111)
    if os.path.lexists(binary) and (not binary.is_symlink() or binary.resolve() != source):
        raise SystemExit('Another uva-opencode command exists. It was left unchanged.')
    if not binary.exists():
        binary.symlink_to(source)
    print('Installed. Run uva-opencode in your project folder. First launch asks for UvA login.')
    print('If the command is not found, add ~/.local/bin to your PATH or run ' + str(binary))

if __name__ == '__main__':
    main()
