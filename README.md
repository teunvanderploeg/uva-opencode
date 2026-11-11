# UvA OpenCode

Run OpenCode against UvA AI Chat through a dedicated local Chromium session.
The helper is written in Rust. Native bundles include Chromium and the official
OpenCode binary, so users do not need Python, Playwright, or a separate OpenCode
installation.

Before upgrading an existing installation, close OpenCode and run
`uva-opencode --stop` so the installer can replace its executables and browser.

## Install

Native packages target macOS ARM64 and Intel, Windows x64, and Linux x64. The
[installer workflow](.github/workflows/native-build.yml) builds and installs each
package on its target OS. It requires a manual Actions run and does not publish
releases automatically. Local verification results and checks that still need
other machines are recorded in [rust-validation.json](rust-validation.json).

- **macOS:** Open the `.pkg` for your processor. Current packages are unsigned
  and not notarized. Open a new terminal after installation.
- **Windows:** Run the `-setup.exe` installer, then open a new PowerShell window.
- **Linux:** Extract the `.tar.gz` and run its `install.sh`. Ensure
  `~/.local/bin` is on PATH. Chromium needs the system libraries present on a
  supported Linux desktop; the CI workflow lists the Ubuntu build dependencies.

In a project folder, run:

```sh
uva-opencode
```

The first launch opens Chromium for your normal UvA login and MFA. Keep the
window open until the helper closes it. The helper then verifies that it can
reuse the same profile headlessly. Later launches reuse that background session.
Select a UvA model in OpenCode with `/models`.

## Commands

```sh
uva-opencode /path/to/project
uva-opencode --login   # Verify login or retry a failed login flow
uva-opencode --check   # Connect without launching OpenCode
uva-opencode --stop    # Stop the helper and its browser
uva-opencode --self-test  # Verify a native bundle without a UvA account
```

`--setup` remains an alias for `--login`. `--wait SECONDS` changes the login wait
limit, which defaults to 600 seconds. The launcher preserves the selected
project directory, terminal input/output, and OpenCode's exit code. It creates
an isolated OpenCode profile rather than changing your normal OpenCode config.
`--self-test` always checks the bundled executables and ignores external browser
path overrides.

If a session expires, the helper opens the login window again. Complete sign-in,
then retry the interrupted request. Queued requests from the expired session
fail rather than running after the new login. Closing an unfinished login window
leaves an error that `--login` can recover from.

## Build from source

Install Rust through rustup. `rust-toolchain.toml` pins the toolchain. Build the
self-contained bundle with:

```sh
cargo xtask bundle
```

This uses the committed `vendor-lock.json` to download Chromium and OpenCode,
checks their SHA-256 hashes, collects dependency notices, and runs the bundle's
`--self-test`. The bundle is under `dist/native/uva-opencode/`. Run its
`uva-opencode` executable directly, or `uva-opencode.exe` on Windows.

On macOS and Linux, this command installs a bundle in your account and creates
`~/.local/bin/uva-opencode`:

```sh
cargo xtask install
```

Use `cargo xtask install --prefix /path/to/prefix` to choose a different install
location. It creates `bin/uva-opencode` and `share/uva-opencode-app` there.

The installer refuses to overwrite an unrelated existing command. Stop a running
helper before replacing an installed bundle. The previous app directory is kept
next to the new installation for rollback.

For development, point the Rust executable at the bundle's browser distribution:

```sh
UVA_BROWSERS_DIR="$PWD/dist/native/uva-opencode/browsers" cargo run -- --check
UVA_BROWSERS_DIR="$PWD/dist/native/uva-opencode/browsers" cargo run --
```

An existing development download in `.runtime/browsers/` is also recognized.
Development runs use `.runtime/rust/` for private state. `cargo run` uses OpenCode
from PATH unless an OpenCode executable is beside the helper under `vendor/`.

## Native release builds

Build on the target OS and processor:

```sh
cargo xtask native
cargo xtask source
```

Native output uses `.pkg` on macOS, Inno Setup on Windows, and `.tar.gz` on Linux.
The source ZIP contains an explicit list of source/configuration files and source
directories. It excludes private profiles, keys, logs, dependencies, build output,
and the Python implementation.

Vendor versions and download hashes are committed in `vendor-lock.json`. To
update them, change the version constants in `xtask/src/package.rs`, run
`cargo xtask lock-artifacts`, and review the new lock file. That command verifies
OpenCode's npm SHA-512 integrity while generating the lock. Normal builds use
only the committed hashes. The lock covers every supported OS/processor pair.

Keep `THIRD_PARTY.md`, `licenses/`, Chromium notices, and `vendor/` provenance
with redistributed bundles. Signing, notarization, and publication are separate
release steps.

## Tests

The default tests are offline and do not require Chromium or an account:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Browser tests use a local fixture server and disposable profiles. They check
visible-to-headless persistence, browser identity, expired sessions, cancellation,
and shutdown. They need a desktop and a downloaded Chromium:

```sh
cargo test --test browser_lifecycle -- --ignored
cargo test --lib lifecycle_recovers -- --ignored
cargo test --lib closed_login_window -- --ignored
```

The process tests need port 8765 to be free:

```sh
cargo run -- --stop
cargo test --test launcher_process -- --ignored --test-threads=1
```

Live checks use your own UvA account. Start the Rust bridge first, then run:

```sh
cargo run -- --check
cargo xtask live --model gpt-6-luna --scenario copy
cargo xtask live --model claude-sonnet-5.5 --scenario website
cargo xtask models
```

Use `--opencode /path/to/bundled/opencode` to test the bundled client. `--state`
selects another bridge state directory. Both file scenarios create disposable
projects and require the agent to read a random value and write it to an output
file. They verify completed client tools, not just the model's final claim.
Results are written inside the private state directory.

For a fresh-account login check without occupying the bridge port:

```sh
UVA_STATE_DIR="$PWD/.runtime/fresh-login" cargo run --example login
```

This opens the normal UvA login flow, verifies headless reuse and an authenticated
request, then closes its browser. The profile remains private for a later restart
check. Do not run two processes against the same profile.

## How it works

```text
OpenCode -> authenticated loopback API -> dedicated Chromium -> UvA Chat
```

Rust controls Chromium through the Chrome DevTools Protocol over private
inherited pipes. Chromium does
not open a debugging port. On Windows, a launch shim owns the inherited handles
and a job object that closes its browser processes when the helper exits. An
embedded JavaScript function performs same-origin fetches inside the signed-in
page. Rust parses the upstream events, validates tool requests, and returns the
OpenAI-compatible API responses. Browser cookies remain in the dedicated profile;
the bridge does not export them or read your everyday browser's credentials.

The bridge binds only to `127.0.0.1:8765`. Requests need the local bearer token,
correct Host header, and no browser Origin header. Requests are limited to
2,000,000 bytes. The browser queue holds four waiting requests and processes one
upstream request at a time. Statistics retain the last 1,000 successful requests
and contain counts and event types rather than prompt or response text.

UvA's `/api/v1/chat/generic` endpoint is undocumented. External-client support
has not been confirmed by UvA, and website changes can break the integration.
Native client tool calling remains experimental. The default adapter uses a JSON
transport protocol through `systemPrompt`. It checks tool names, argument objects,
and `tool_choice` before returning a tool request to OpenCode. OpenCode executes
the tools under its own permissions. A formatting failure gets at most one repair
request and then HTTP 422. Generated HTML is never searched for embedded tool
instructions.

Only text is supported. Responses are buffered before the compatible SSE output
is sent so tool calls can be validated in full. Token usage and remaining quota
are not reported. The configured 32,000-token context cap is conservative and is
not a measured UvA limit. File contents read by OpenCode can be sent to UvA as
conversation context.

## Private state and Python rollback

Installed state stays in the existing per-user locations:

- macOS: `~/Library/Application Support/UvAOpenCode/`
- Windows: `%LOCALAPPDATA%\UvAOpenCode\`
- Linux: `${XDG_DATA_HOME:-~/.local/share}/uva-opencode/`

`UVA_STATE_DIR` overrides private state. `UVA_BROWSER_PATH` selects an explicit
Chromium executable, and `UVA_BROWSERS_DIR` selects its distribution directory.
Treat profiles and local tokens as private account data. Unix directories use
mode 0700 and token files use 0600. Windows state directories restrict inherited
access to the current account.

The Python implementation remains in the checkout while cross-platform Rust
installation checks finish. Its source setup still uses `python3 install.py`;
its original tests remain available. Rust development state is separate from
Python's `.runtime/browser-profile/`. Stop the older helper before switching:
both implementations use port 8765, and profiles must have a single owner.
The Rust launcher reports an incompatible existing helper instead of silently
reusing it. The old userscript/extension backend is retired from the Rust build.
