# UvA OpenCode

Run OpenCode against UvA AI Chat using a dedicated local browser. No browser
extension or console snippet is needed.

## Install

### Native installer builds

The Mac Apple Silicon installer is generated at
`dist/uva-opencode-0.4.0-macos-arm64.pkg`. It contains its own Python runtime,
Chromium, and a pinned official OpenCode binary. Classmates do not need to
install those dependencies. After installing, open a fresh terminal in a project
folder and run `uva-opencode`. The first run opens the UvA login window.

This is an unsigned installer candidate. It has not been notarized by Apple or
tested through an actual system installation. Bundled browser and OpenCode
startup checks pass, and the existing login backend has passed its live tests.
Apple signing and notarization are needed for a smooth public Mac release.

The [installer workflow](.github/workflows/native-build.yml) builds and tests
packages on Apple Silicon and Intel Macs, Windows x64, and Ubuntu x64.
Download a package from a successful Actions run, or build it locally.
The workflow does not publish a release automatically.

Build dependencies, needed only by the person producing installers:

```sh
python -m pip install -r requirements.txt pyinstaller==6.22.0
python build_native.py
```

Build on each target OS and CPU architecture. The native bundle's `--self-test`
starts its included Chromium and OpenCode without using an external Python or
OpenCode installation. Packaged credentials and profiles are stored in the
user's application-data directory, outside the immutable install directory.

### Source installation

Requirements: macOS or Linux, Python 3, and an installed `opencode` command.
Linux may also need Playwright's system browser dependencies.

Extract the source archive, open a terminal in its folder, and run:

```sh
python3 install.py
```

The installer creates a private Python environment, installs a pinned Playwright
version and Chromium runtime, and adds `~/.local/bin/uva-opencode`. If that folder
is not on your PATH, the installer prints the full command to use. Keep the source
folder after installation because the command points to it.

## Use

```sh
cd /path/to/project
uva-opencode
```

On first use, a Chromium window opens for your normal UvA login and MFA. Leave
it open until the helper closes it. The helper then checks that the session can
be reused by a background browser. Later launches reuse that profile and need
no visible window while the session remains valid.

The visible window closes, but a browser process continues running to make
requests. Your everyday Firefox or Chrome can be closed. Login credentials are
never read from another browser, exported into the bridge, or shipped with the
installer. The dedicated browser keeps its own normal cookies on disk.

The launcher preserves the folder you run it from. It automatically starts the
local bridge and uses a separate OpenCode profile. Select a UvA model with
`/models`. Seventeen models are enabled; two legacy Claude entries failed the
last endpoint check and are disabled in `opencode.example.json`.

Additional commands:

```sh
uva-opencode --login   # Verify login, or reopen a failed login flow
uva-opencode --check   # Verify the background connection without opening OpenCode
uva-opencode --stop    # Stop the helper and its browser
```

If login expires, the helper opens the login window again. After signing in,
retry the interrupted OpenCode request. The installed app keeps the browser
profile in your account's application data directory. Source installs use
`.runtime/browser-profile/`. Treat either profile as private account data.

## How it works

```text
OpenCode -> authenticated loopback API -> dedicated browser -> UvA Chat
```

The browser uses the existing `/api/v1/chat/generic` endpoint. This is an
undocumented UvA interface, and external-client support has not been confirmed
by UvA. A website change can break the integration.

Native client tool calling did not work in the initial check. The adapter uses
an explicit JSON tool protocol through UvA's `systemPrompt` field. OpenCode
executes the requested tools under its own permissions and returns the results.
A formatting failure gets at most one correction request, then returns HTTP 422.
This remains emulated tool calling. It is less reliable than a supported native
coding API.

File contents read by OpenCode can be sent to UvA as conversation context.
Only text is supported. Responses are buffered before returning the OpenAI-style
stream. Token usage and remaining allowance are not reported to OpenCode; a
zero displayed there is not a measurement of actual UvA usage. The 32,000-token
client context cap is conservative and is not a measured upstream limit.

## Checks

```sh
python3 -m unittest -v test_bridge.py test_launch.py test_browser_session.py
python3 test_opencode.py --model claude-sonnet-5.5 --scenario website
```

The website regression checks that the real OpenCode Build agent reads a random
value from a test file and creates HTML containing that value in a disposable
project. GPT file reading and writing were also verified before this backend
change. Current login and end-to-end verification results are recorded in
`validation.json`.

## Share with classmates

Share the native installer that matches each classmate's OS and CPU. Each person
signs in using their own UvA account. For source installs, run
`python3 make_release.py`. The resulting `dist/uva-opencode-source.zip`
contains only explicitly listed source and installation files. It excludes local
keys, login profiles, logs, Python dependencies, and downloaded browsers.

## Previous extension setup

The earlier Firefox userscript is not used by the default backend. You may
disable **UvA OpenCode relay** in Tampermonkey. The old source files remain for
reference but are excluded from the distributable archive.
