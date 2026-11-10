"""Build a source installer archive using an explicit allowlist. No login state."""
from pathlib import Path
import zipfile

ROOT = Path(__file__).resolve().parent
FILES = ['launch.py', 'bridge.py', 'browser_session.py', 'install.py', 'requirements.txt',
         'runtime_paths.py',
         'opencode.example.json', 'README.md', 'validation.json', 'probe.py',
         'test_bridge.py', 'test_launch.py', 'test_browser_session.py', 'test_opencode.py',
         'make_release.py']

def build():
    destination = ROOT / 'dist/uva-opencode-source.zip'
    destination.parent.mkdir(exist_ok=True)
    with zipfile.ZipFile(destination, 'w', zipfile.ZIP_DEFLATED) as archive:
        for name in FILES:
            archive.write(ROOT / name, 'uva-opencode/' + name)
    return destination

if __name__ == '__main__':
    print(build())
