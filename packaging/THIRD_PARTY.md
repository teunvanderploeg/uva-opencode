# Included third-party software

This unofficial UvA helper includes a Python runtime, Playwright and its Node
driver, greenlet, Chromium, and OpenCode. Their distribution licenses and notices
must travel with the bundle. Runtime licenses are collected under `licenses/`;
Chromium notices remain in its browser directory. OpenCode is fetched from its
official npm distribution with the registry SHA-512 integrity checked; package
provenance and any published notices are under `vendor/`.

PyInstaller's licensing exception permits distribution of frozen applications.
It does not replace the licenses of bundled dependencies. Review the actual
bundle's notices before publishing a release. This helper is not a UvA product.
