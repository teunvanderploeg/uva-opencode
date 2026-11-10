"""Build on the target OS; end users need no Python, Node, or OpenCode install."""
import argparse
import base64
import hashlib
import io
import json
import os
import platform
import plistlib
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import urlopen

ROOT = Path(__file__).resolve().parent
VERSION = '0.4.0'
OPENCODE = '2.2.5'

def fetch_opencode(destination):
    system = {'Darwin':'darwin', 'Linux':'linux', 'Windows':'windows'}[platform.system()]
    arch = {'arm64':'arm64', 'aarch64':'arm64', 'x86_64':'x64', 'AMD64':'x64'}[platform.machine()]
    target = system + '-' + arch + ('-baseline' if system in ('linux','windows') and arch == 'x64' else '')
    metadata = None
    for scope in ('@opencode','@opencode-ai'):
        url = 'https://registry.npmjs.org/' + scope + '%2fcli-' + target + '/' + OPENCODE
        try:
            with urlopen(url) as response:
                metadata = json.load(response)
            break
        except HTTPError as error:
            if error.code != 404:
                raise
    if metadata is None:
        raise RuntimeError('Pinned OpenCode package was not found for ' + target)
    with urlopen(metadata['dist']['tarball']) as response:
        data = response.read()
    integrity = metadata['dist']['integrity']
    algorithm, expected = integrity.split('-', 1)
    if algorithm != 'sha512' or base64.b64encode(hashlib.sha512(data).digest()).decode() != expected:
        raise RuntimeError('OpenCode package integrity verification failed.')
    destination.mkdir(parents=True, exist_ok=True)
    name = 'opencode.exe' if system == 'windows' else 'opencode'
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
        binary = next(m for m in archive.getmembers() if Path(m.name).name == name and m.isfile())
        (destination / name).write_bytes(archive.extractfile(binary).read())
        (destination / name).chmod(0o755)
        for member in archive.getmembers():
            if member.isfile() and Path(member.name).name.lower().startswith(('license','notice','copying')):
                (destination / Path(member.name).name).write_bytes(archive.extractfile(member).read())
    license_url = 'https://raw.githubusercontent.com/anomalyco/opencode/v' + OPENCODE + '/LICENSE'
    with urlopen(license_url) as response:
        (destination / 'OpenCode-LICENSE.txt').write_bytes(response.read())
    (destination / 'provenance.json').write_text(json.dumps({
        'package':metadata['name'], 'version':OPENCODE, 'integrity':integrity,
        'source':metadata['dist']['tarball'], 'license':metadata.get('license'),
        'license_source':license_url}, indent=2))

def build():
    parser = argparse.ArgumentParser()
    parser.add_argument('--browser-source', type=Path)
    args = parser.parse_args()
    work = ROOT / '.runtime/native-build'
    work.mkdir(parents=True, exist_ok=True)
    browsers = args.browser_source or work / 'browsers'
    environment = os.environ.copy()
    environment['PLAYWRIGHT_BROWSERS_PATH'] = str(browsers.resolve())
    if not args.browser_source:
        subprocess.run([sys.executable,'-m','playwright','install','chromium'],env=environment,check=True)
    subprocess.run([sys.executable,'-m','PyInstaller','--noconfirm','--clean','--onedir',
        '--name','uva-opencode','--distpath',str(ROOT/'dist/native'),
        '--workpath',str(work/'pyinstaller'),'--specpath',str(work),
        '--add-data',str(ROOT/'opencode.example.json') + os.pathsep + '.',
        '--collect-all','playwright', str(ROOT/'frozen_entry.py')],check=True,env=environment,cwd=ROOT)
    bundle = ROOT/'dist/native/uva-opencode'
    shutil.copytree(browsers,bundle/'browsers',dirs_exist_ok=True,symlinks=True,
                    ignore=shutil.ignore_patterns('.links','chromium_headless_shell-*','ffmpeg-*'))
    fetch_opencode(bundle/'vendor')
    shutil.copyfile(ROOT/'packaging/THIRD_PARTY.md',bundle/'THIRD_PARTY.md')
    # Retain runtime distribution notices; no developer profile is copied.
    import importlib.metadata
    notices = bundle/'licenses'
    notices.mkdir(exist_ok=True)
    with urlopen('https://raw.githubusercontent.com/python/cpython/v' + platform.python_version() + '/LICENSE') as response:
        (notices/'Python-LICENSE.txt').write_bytes(response.read())
    for package in ('playwright','greenlet','pyinstaller'):
        distribution = importlib.metadata.distribution(package)
        for item in distribution.files or []:
            if 'license' in str(item).lower() or Path(str(item)).name.lower().startswith('copying'):
                source = distribution.locate_file(item)
                if source.is_file():
                    shutil.copyfile(source,notices/(package+'-'+Path(str(item)).name))
    binary = bundle/('uva-opencode.exe' if os.name=='nt' else 'uva-opencode')
    subprocess.run([str(binary),'--self-test'],check=True)
    if sys.platform=='darwin':
        payload=work/'pkgroot'
        install=payload/'Library/Application Support/UvAOpenCodeApp'
        if payload.exists():shutil.rmtree(payload)
        shutil.copytree(bundle,install,symlinks=True)
        components = work/'components.plist'
        subprocess.run(['pkgbuild','--analyze','--root',str(payload),str(components)],check=True)
        data = plistlib.loads(components.read_bytes())
        def nonrelocatable(value):
            if isinstance(value,dict):
                if 'BundleIsRelocatable' in value:
                    value['BundleIsRelocatable'] = False
                for child in value.values():nonrelocatable(child)
            elif isinstance(value,list):
                for child in value:nonrelocatable(child)
        nonrelocatable(data)
        components.write_bytes(plistlib.dumps(data))
        subprocess.run(['pkgbuild','--root',str(payload),'--scripts',str(ROOT/'packaging/macos'),
                        '--component-plist',str(components),
                        '--identifier','nl.uva.unofficial.uva-opencode','--version',VERSION,
                        str(ROOT/f'dist/uva-opencode-{VERSION}-macos-{platform.machine()}.pkg')],check=True)
    elif os.name=='nt':
        compiler = shutil.which('iscc') or str(Path(os.environ['ProgramFiles(x86)'])/'Inno Setup 6/ISCC.exe')
        subprocess.run([compiler,str(ROOT/'packaging/windows.iss')],check=True,cwd=ROOT)
    else:
        stage=work/'linux-package'
        if stage.exists():shutil.rmtree(stage)
        shutil.copytree(bundle,stage/'app',symlinks=True)
        shutil.copyfile(ROOT/'packaging/install-linux.sh',stage/'install.sh')
        (stage/'install.sh').chmod(0o755)
        with tarfile.open(ROOT/f'dist/uva-opencode-{VERSION}-linux-{platform.machine()}.tar.gz','w:gz') as archive:
            archive.add(stage,arcname='uva-opencode')
    print('Native build and self-test completed for',platform.system(),platform.machine())

if __name__=='__main__':build()
