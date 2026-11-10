#!/usr/bin/env python3
"""Start the dedicated UvA browser and launch OpenCode in the calling folder."""
import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
from urllib.error import URLError
from urllib.request import Request, urlopen

from runtime_paths import ROOT, STATE, APP, FROZEN, BROWSERS, file_lock
BASE = 'http://127.0.0.1:8765'


def api(path, payload=None):
    token = (STATE / 'local-token').read_text().strip()
    req = Request(BASE + path, data=None if payload is None else json.dumps(payload).encode(),
                  headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
    with urlopen(req, timeout=3) as response:
        return json.load(response)


def health():
    try:
        value = api('/health')
        if value.get('service') != 'uva-opencode' or value.get('protocol') not in (2, 3):
            raise RuntimeError('An older bridge is running. Stop it with Ctrl+C before starting this version.')
        return value
    except (FileNotFoundError, URLError, TimeoutError):
        return None


def ensure_bridge():
    STATE.mkdir(mode=0o700, exist_ok=True)
    STATE.chmod(0o700)
    with file_lock(STATE / 'launch.lock'):
        existing = health()
        if existing and existing.get('protocol') == 2:
            api('/shutdown', {})
            for _ in range(30):
                time.sleep(0.2)
                if health() is None:
                    break
            existing = health()
        if existing:
            return existing
        python = ROOT / '.venv' / ('Scripts/python.exe' if os.name == 'nt' else 'bin/python')
        if not FROZEN and not python.exists():
            raise RuntimeError('Run python3 install.py once to install the browser helper.')
        environment = os.environ.copy()
        environment['PLAYWRIGHT_BROWSERS_PATH'] = str(BROWSERS)
        command = ([sys.executable, '--internal-bridge'] if FROZEN else [str(python), str(ROOT / 'bridge.py')])
        flags = {'creationflags': subprocess.CREATE_NEW_PROCESS_GROUP} if os.name == 'nt' else {'start_new_session': True}
        with (STATE / 'bridge.log').open('ab') as log:
            process = subprocess.Popen(command + ['--tool-mode', 'prompt'],
                                       cwd=ROOT, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                       env=environment, **flags)
        for _ in range(60):
            status = health()
            if status:
                return status
            if process.poll() is not None:
                raise RuntimeError('The bridge could not start. See .runtime/bridge.log. Port 8765 may be busy.')
            time.sleep(0.1)
        process.terminate()
        raise RuntimeError('The bridge did not start within six seconds. See .runtime/bridge.log.')


def wait_for_relay(timeout):
    print('Connecting to UvA Chat...', flush=True)
    deadline = time.monotonic() + timeout
    prompted = False
    while time.monotonic() < deadline:
        status = health()
        if status and status['ready']:
            return
        if status and status.get('browser_state') == 'error':
            raise RuntimeError(status.get('browser_error') or 'Browser helper failed.')
        if status and status.get('browser_state') == 'login_required' and not prompted:
            print('Sign in with UvA in the Chromium window. It will close when login is complete.', flush=True)
            prompted = True
        time.sleep(0.5)
    raise RuntimeError('Login did not finish in time. Run uva-opencode --login to try again.')


def opencode_environment(directory):
    config_dir = STATE / 'opencode-profile/config/opencode'
    config_dir.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(ROOT / 'opencode.example.json', config_dir / 'opencode.json')
    environment = os.environ.copy()
    for key in list(environment):
        if key.startswith('OPENCODE_'):
            environment.pop(key)
    environment['UVA_BRIDGE_KEY'] = (STATE / 'local-token').read_text().strip()
    for name, leaf in [('XDG_DATA_HOME', 'data'), ('XDG_CONFIG_HOME', 'config'),
                       ('XDG_CACHE_HOME', 'cache'), ('XDG_STATE_HOME', 'state')]:
        environment[name] = str(STATE / 'opencode-profile' / leaf)
    environment['PWD'] = str(directory)
    return environment


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('directory', nargs='?', default='.')
    parser.add_argument('--setup', '--login', dest='setup', action='store_true', help='Open or verify your dedicated UvA login.')
    parser.add_argument('--check', action='store_true', help='Connect the relay without launching OpenCode.')
    parser.add_argument('--stop', action='store_true', help='Stop this local bridge.')
    parser.add_argument('--no-browser', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--wait', type=float, default=600, metavar='SECONDS')
    args = parser.parse_args()
    if args.wait <= 0:
        parser.error('--wait must be positive')
    if args.stop:
        if health():
            api('/shutdown', {})
            print('UvA bridge stopped.')
        else:
            print('UvA bridge is not running.')
        return 0
    directory = Path(args.directory).expanduser().resolve(strict=True)
    if not directory.is_dir():
        raise RuntimeError('Choose a project directory.')
    executable = shutil.which('opencode')
    packaged = APP / 'vendor' / ('opencode.exe' if os.name == 'nt' else 'opencode')
    if FROZEN and packaged.is_file():
        executable = str(packaged)
    fallback = Path.home() / '.opencode/bin/opencode'
    if not executable and fallback.is_file():
        executable = str(fallback)
    if not args.setup and not args.check and not executable:
        raise RuntimeError('OpenCode must be installed and on PATH.')
    if args.setup:
        status = health()
        if status and status.get('browser_state') == 'error':
            api('/shutdown', {})
            time.sleep(1)
    status = ensure_bridge()
    if not status['ready']:
        wait_for_relay(args.wait)
    print('UvA connected. Browser running in the background.', flush=True)
    if args.check or args.setup:
        return 0
    return subprocess.call([executable, '--standalone'], cwd=directory,
                           env=opencode_environment(directory))


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        print('\nStopped. The bridge stays available for the next launch.', file=sys.stderr)
        raise SystemExit(130)
    except (RuntimeError, OSError) as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1)
