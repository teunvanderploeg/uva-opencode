# Included third-party software

The Rust UvA OpenCode bundle includes Chromium and the official OpenCode binary.
Chromium's notices remain inside its browser distribution. OpenCode's license
and the pinned download provenance are under `vendor/`.

The `licenses/` directory contains Cargo dependency license files and a package
license manifest. Rust runtime notices are copied from the build toolchain when
available. Keep these files with redistributed bundles.

Vendor downloads use versions and SHA-256 hashes from `vendor-lock.json`.
OpenCode's npm SHA-512 integrity is also verified when creating that lock.
The bundle does not include Python, Playwright, or Playwright's Node driver.
OpenCode may include its own runtime dependencies.

This helper is unofficial and is not a UvA product. The existing Python build
remains in the repository for rollback while other-platform validation finishes.
