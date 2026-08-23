# Code Signing Policy

This document describes how KimiCodeBar for Windows signs and distributes its release artifacts.

**Status: Application pending** — this project has applied to the [SignPath Foundation](https://signpath.org/) free code signing program for open-source projects. Until approved, releases remain unsigned.

## What will be signed

- Windows installer: `KimiCodeBar_<version>_x64-setup.exe` (NSIS, per-user install)
- Windows portable: `kimicodebar.exe` inside `KimiCodeBar_<version>_x64-portable.zip`

Published on GitHub Releases: <https://github.com/JYH1878/KimiCodeBar-Windows/releases>

## Build and signing process

- All release artifacts are built from the public repository by GitHub Actions (`release.yml`), triggered by a version tag (`vX.Y.Z`).
- Only CI-built artifacts are submitted to SignPath for signing. Manually built or uploaded binaries are never signed.
- The private key is held by SignPath (HSM-backed). This repository never stores private keys or certificate material.

## Roles (single-maintainer project)

- **Maintainer** (commit + release access): JYH1878 — creates release tags, triggers releases, approves every signing request.
- **Reviewer**: JYH1878 — all external pull requests are reviewed by the maintainer before merge.
- **Approver**: JYH1878 — each signing request requires explicit approval by the maintainer.

## Distribution channels

- GitHub Releases: <https://github.com/JYH1878/KimiCodeBar-Windows/releases>
- Scoop: <https://github.com/JYH1878/scoop-bucket> (portable build)
- winget: `JYH1878.KimiCodeBar` (installer build, major releases only)

## Privacy

This program does not transfer any information to other networked systems unless specifically requested by the user. See [PRIVACY.md](PRIVACY.md).
