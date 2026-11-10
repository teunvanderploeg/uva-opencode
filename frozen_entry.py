"""Native executable entry point. No external Python needed at runtime."""
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from runtime_paths import APP, BROWSERS

def self_test():
    from playwright.sync_api import sync_playwright
    os.environ['PLAYWRIGHT_BROWSERS_PATH'] = str(BROWSERS)
    with tempfile.TemporaryDirectory(prefix='uva-binary-check-') as folder:
        with sync_playwright() as p:
            context = p.chromium.launch_persistent_context(folder, channel='chromium', headless=True,
                                                          chromium_sandbox=True)
            page = context.pages[0]
            assert page.evaluate('1 + 1') == 2
            context.close()
        env = os.environ.copy()
        for name in ('XDG_DATA_HOME','XDG_STATE_HOME','XDG_CACHE_HOME','XDG_CONFIG_HOME'):
            env[name] = str(Path(folder) / name)
        binary = APP / 'vendor' / ('opencode.exe' if os.name == 'nt' else 'opencode')
        result = subprocess.run([str(binary), '--version'], capture_output=True, text=True, env=env, timeout=30)
        if result.returncode:
            raise RuntimeError('Bundled OpenCode did not start.')
        print(json.dumps({'bundled_browser': 'passed', 'bundled_opencode': result.stdout.strip(),
                          'external_python_required': False}))

if __name__ == '__main__':
    if '--internal-bridge' in sys.argv:
        sys.argv.remove('--internal-bridge')
        from bridge import main
        main()
    elif '--self-test' in sys.argv:
        self_test()
    else:
        from launch import main
        try:
            raise SystemExit(main())
        except KeyboardInterrupt:
            raise SystemExit(130)
        except (RuntimeError, OSError) as error:
            print(str(error), file=sys.stderr)
            raise SystemExit(1)
