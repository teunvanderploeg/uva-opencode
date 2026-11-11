# UvA OpenCode

Run OpenCode in any project folder using your UvA AI Chat account.
It bundles Chromium and OpenCode, so you do not need Python or a separate OpenCode installation.

> Unofficial project. Requires a UvA account with access to [UvA AI Chat](https://aichat.uva.nl).

## Install

**Prebuilt Rust installers are not published yet.** Install from source using
[Git](https://git-scm.com/install/) and [Rust](https://rustup.rs/), then open a new terminal.
The repository selects the required Rust version automatically.

If upgrading, quit OpenCode and run `uva-opencode --stop` first.

### macOS and Linux

Run these commands in Terminal:

```sh
git clone https://github.com/teunvanderploeg/uva-opencode.git
cd uva-opencode
cargo xtask install
export PATH="$HOME/.local/bin:$PATH"
```

To keep the command available in new terminals, add the `export PATH` line to
`~/.zshrc` for Zsh, or `~/.bashrc` for Bash.
Linux x64 also needs Chromium's desktop libraries; the [build workflow](.github/workflows/native-build.yml) lists the Ubuntu packages.

<details>
<summary>Windows x64 installation</summary>

If Rust setup asks for Visual Studio C++ build tools, install them first.
Then build and check the application in PowerShell:

```powershell
git clone https://github.com/teunvanderploeg/uva-opencode.git
cd uva-opencode
cargo xtask bundle
.\dist\native\uva-opencode\uva-opencode.exe --self-test
(Resolve-Path .\dist\native\uva-opencode).Path
```

Copy the displayed path. In the Start menu, open "Edit environment variables for
your account", edit **Path**, and add it as a new entry. Open a new PowerShell
window afterward so `uva-opencode` is available in any folder.

</details>

macOS ARM64 has been tested locally. Windows, Linux, and native Intel Mac
installation checks remain outstanding; see the [validation record](rust-validation.json).

## Start coding

Open a terminal in your project folder and run:

```sh
cd /path/to/your/project
uva-opencode
```

1. Complete UvA sign-in and MFA in Chromium if prompted. Leave the window open until the helper closes it.
2. Select a model in OpenCode with `/models`.
3. Ask OpenCode to read, edit, or create files in your project.

Later launches reuse your saved login. OpenCode uses its own isolated configuration.

## Useful commands

| Command | What it does |
| --- | --- |
| `uva-opencode --login` | Verify sign-in or retry a failed login. |
| `uva-opencode --check` | Check the connection without opening OpenCode. |
| `uva-opencode --self-test` | Check bundled Chromium and OpenCode without signing in. |
| `uva-opencode --stop` | Stop the background helper and Chromium. |

If your login expires, finish sign-in again and retry the request.

## Before you use it

Only text is supported. Files read by OpenCode may be sent to UvA as conversation
context. Login cookies stay in a separate browser profile. This integration uses
an undocumented UvA endpoint, so changes to UvA Chat can break it.

<details>
<summary>Development and packaging</summary>

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo xtask native   # Build an installer on the target OS and processor
cargo xtask source  # Create a source ZIP
```

The [build workflow](.github/workflows/native-build.yml) includes browser and
installed-package checks. macOS installers are unsigned and not notarized.
Vendor downloads are pinned in [vendor-lock.json](vendor-lock.json).
The Python source remains available for rollback.

</details>
