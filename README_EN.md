# KimiCodeBar for Windows

**[中文](README.md) | English**

[![GitHub release](https://img.shields.io/github/v/release/JYH1878/KimiCodeBar-Windows)](https://github.com/JYH1878/KimiCodeBar-Windows/releases)
[![CI](https://github.com/JYH1878/KimiCodeBar-Windows/actions/workflows/ci.yml/badge.svg)](https://github.com/JYH1878/KimiCodeBar-Windows/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%20%2F%2011-blue)](https://github.com/JYH1878/KimiCodeBar-Windows)

A Windows system tray monitor for Kimi Code usage quotas. Sits quietly in the tray; one left-click shows your quota, and it turns red to warn you before you run out.

Built with **Tauri 2 + Rust + React**: installer under 5 MB, ~10–20 MB RAM in daily use. **Bilingual UI (Chinese / English)** since v0.4.0.

> Community project, not an official Kimi product.
>
> **Official site**: [jyh1878.github.io/KimiCodeBar-Windows](https://jyh1878.github.io/KimiCodeBar-Windows/)
>
> **On macOS? Use the original [xifandev/KimiCodeBar](https://github.com/xifandev/KimiCodeBar)** (maintained by @xifandev, [official site](https://xifandev.github.io/KimiCodeBar/)) — this repo is the author-endorsed community port for Windows; the two repos are maintained and released independently.

## Features

- **System tray app**: left-click pops up the usage panel (positioned above the tray icon, auto-hides on focus loss); right-click menu (Refresh / Settings / Exit)
- **Dual quota windows**: used percentage, remaining amount and reset countdown for both the 7-day and 5-hour rolling windows
- **Membership & wallet**: shows membership tier (Andante / Moderato / Allegretto / Allegro), Booster wallet balance and monthly usage
- **Monthly totals**: combined Kimi + Code monthly usage from the web console (paste the web refresh_token once in Settings; the app renews it automatically)
- **Local token stats**: scans local `wire.jsonl` session logs for per-day token consumption (today/yesterday + 7-day bar chart + today's per-model breakdown) — no API needed; tracked per account (auto-attributed by CLI credentials, covering every `~/.kimi-code-*` home from `KIMI_CODE_HOME` multi-instance setups); also picks up Claude Code, Codex, OpenCode and ZCode (Zhipu's own CLI) local logs, attributed to registered accounts by exact API-key match; multiple keys of the same account used across tools can be registered as "extra keys" in Settings (up to 5 per account) to aggregate usage — keys are matched in memory only, never persisted or sent anywhere; also auto-discovers Kimi Code homes inside local WSL distros (reads distro names from the registry, scans `\\wsl.localhost\<distro>\home\*\.kimi-code` and `root\.kimi-code`, same credential attribution, no setup needed); GLM accounts get the same treatment (GLM keys from Kimi Code custom provider sections and from ZCode's Coding Plan are matched against registered account keys; a one-time full rescan on upgrade backfills history)
- **Usage trend chart**: 24-hour dual-line history (7-day / 5-hour), recorded locally — facts only, no prediction; detail pages annotate today's usage with "(X.X% of weekly quota)", the same-day delta of the official weekly quota (Kimi / GLM; not shown for balance-only accounts like DeepSeek)
- **Light / dark theme**: follows the system or switch manually
- **Panel background**: preset gradients (Night / Aurora / Violet / Ember, each adapted to light & dark themes) or a custom image (PNG / JPG / WebP, ≤10MB), with translucent frosted cards
- **Panel mascot**: an animated blue dumpling (vector SVG) in the bottom-left corner of the panel — blinks and glances around, adapts to light/dark themes; purely decorative
- **Multi-account**: up to 10 accounts, one per panel page with a phone-style horizontal pager (wheel / drag / dot navigation, plus a trailing "+" for quick add); manage accounts in Settings (rename / reorder / delete). Legacy single-account data migrates to "Account 1" automatically on upgrade
- **Overview page**: the panel opens on an "Overview" page that shows every account at a glance — Kimi / GLM accounts get two mini progress bars (5-hour & 7-day: used %, remaining and reset countdown), DeepSeek accounts show their total balance and currency; low-quota accounts are highlighted in red; click any row to jump straight to that account's detail page, with wheel / drag / dot paging unchanged. In minimal mode it collapses to one line of key numbers per account
- **DeepSeek balance**: DeepSeek platform accounts join as a new account type (up to 10 accounts across all providers) — paste a platform API key to get a dedicated panel page showing total balance, granted / topped-up breakdown, currency and account status; low balance (threshold default ¥5, configurable) or an unavailable account triggers the same red alert
- **GLM Coding Plan quota**: Zhipu GLM Coding Plan accounts join as a new account type — paste a platform API key to get a dedicated panel page with 5-hour / weekly quota windows (credit-based plans show real credit balances), plan tier and reset countdown; low remaining triggers the same red alert
- **Two sign-in methods**
  - **API Key**: paste a `sk-kimi-` key, stored in Windows Credential Manager
  - **Account authorization**: OAuth device flow (same as the Kimi Code CLI), one-click browser authorization with automatic token refresh
  - Credentials are configured per account; each account can also paste a web refresh_token to view its monthly totals
- **Adaptive refresh**: polls every 5 minutes by default (1–60 min configurable); tightens to 1-minute polling while a local Kimi session is active (new token usage within the last 10 minutes) and relaxes back when idle — switchable to fixed mode in Settings (adaptive by default)
- **Low-quota alerts**: the tray icon turns red and a system notification fires (naming the account) when any account's window drops below the threshold (default 20%, configurable); failed fetches never trigger a false alert
- **Works offline**: last successful result is cached locally and shown without errors when the network is down
- **Auto update**: silently checks GitHub Releases; an update badge appears in the panel when a new version is available
- **Global hotkey**: summon/dismiss the panel anywhere (e.g. `Ctrl+Shift+K`, record it by pressing the combo in Settings, off by default)
- **CLI mode**: `kimicodebar.exe --status` prints quota JSON for scripts and CI (exit codes 0/1/2)
- **Kimi Code status line**: one toggle in Settings pins your quota to the Kimi Code terminal footer (model and cwd kept, 5-hour / 7-day remaining and plan tier appended, ⚠ prefix when low); reads the local cache with zero network cost and wires up all `~/.kimi-code-*` CLI homes automatically
- **Usage export**: one-click CSV/JSON export of your usage history for expense reports and reviews
- **Launch at login**: optional, one toggle in Settings

## Screenshots

<p>
  <img src="docs/screenshots/panel.png" width="230" alt="Usage panel">
  &nbsp;&nbsp;
  <img src="docs/screenshots/settings1.png" width="330" alt="Settings (sign-in & general)">
  &nbsp;&nbsp;
  <img src="docs/screenshots/settings2.png" width="330" alt="Settings (diagnostics & logs)">
</p>

*(Screenshots show the light theme; dark theme and English UI are available in Settings.)*

## Install

**Option 1 · Installer (recommended)**: download `KimiCodeBar_x.x.x_x64-setup.exe` from [Releases](https://github.com/JYH1878/KimiCodeBar-Windows/releases) and run it — per-user install, **no admin rights required**, appears in the Start Menu.

**Option 2 · Portable**: download `KimiCodeBar_x.x.x_x64-portable.zip`, extract anywhere and run `kimicodebar.exe`. No registry writes. Note: settings and cache still live in `%APPDATA%\KimiCodeBar\` (set the `KIMICODEBAR_CONFIG_DIR` environment variable to relocate them).

**Option 3 · Scoop**: `scoop bucket add kimicodebar https://github.com/JYH1878/scoop-bucket`, then `scoop install kimicodebar/kimicodebar` (installs the portable build; config still lives in `%APPDATA%\KimiCodeBar\`; upgrade later with `scoop update kimicodebar`).

**Option 4 · winget**: `winget install JYH1878.KimiCodeBar` (installs the installer build — per-user, no admin rights; upgrade later with `winget upgrade JYH1878.KimiCodeBar`).

On first launch the icon sits in the system tray (possibly inside the "hidden icons" overflow — drag it out).

Requirements: Windows 10 1809+ / Windows 11; WebView2 Runtime (preinstalled on Windows 11; the installer will prompt if missing).

> Windows SmartScreen may warn on first run (the app is not yet code-signed — a common situation for open-source software): click "More info" → "Run anyway".

## Usage

### First-time setup (choose one)

**Option A · API Key**
1. Create an API key at [kimi.com/code/console](https://www.kimi.com/code/console) (starts with `sk-kimi-`)
2. Tray right-click → Settings → Method A → paste the key → Save

**Option B · Account authorization**
1. Tray right-click → Settings → Method B → "Start authorization"
2. Click "Open browser to authorize" and confirm the code on the web page
3. Signed in — tokens refresh automatically

### Daily use

- **Check quota**: left-click the tray icon
- **Manual refresh**: the refresh button on the panel, or tray right-click → Refresh
- **Settings**: refresh interval, alert threshold, alert toggle, launch at login, global hotkey, language

### CLI mode

```bash
kimicodebar.exe --status          # prints quota JSON using stored credentials
set KIMI_API_KEY=sk-kimi-xxx && kimicodebar.exe --status   # CI/scripts via env var
```

Exit codes: `0` success, `1` network/API error, `2` no credential. Errors go to stderr as JSON; stdout stays clean for scripting.

## FAQ

**Is a `sk-kimi-` key the same as an `sk-` key from the open platform?**

No. Kimi Code keys (kimi.com/code/console) start with `sk-kimi-`; the open platform (platform.moonshot.cn) issues `sk-` keys for the general LLM API. They are not interchangeable. This app only accepts `sk-kimi-` keys (validated in Settings).

**What are the Andante / Moderato / Allegretto / Allegro tiers?**

Kimi's official membership tier names (musical tempos, slowest to fastest = lowest to highest). Shown as returned by the API.

**What is Booster?**

Kimi Code's pay-as-you-go wallet: when your subscription quota runs out, a prepaid balance can keep you going. The card shows "Not activated" if you haven't enabled it; otherwise it shows balance and monthly usage/limit.

**What happens offline?**

The panel shows the last cached result with a non-blocking banner — no errors, no crashes. It refreshes automatically once the network is back.

**Is my data uploaded anywhere?**

No. All credentials and settings stay on your machine (API key in Windows Credential Manager; everything else in `%APPDATA%\KimiCodeBar\`). Network requests go only to: `api.kimi.com` (usage), `auth.kimi.com` (authorization), `api.deepseek.com` (DeepSeek balance, only if a DeepSeek account is configured), `open.bigmodel.cn` (GLM quota, only if a GLM account is configured), `api.github.com` (update checks). See [PRIVACY.md](PRIVACY.md) and [CODE_SIGNING_POLICY.md](CODE_SIGNING_POLICY.md).

## Build from source

Prerequisites: [Rust](https://rustup.rs/) (1.77+), [Node.js](https://nodejs.org/) (18+)

```bash
git clone https://github.com/JYH1878/KimiCodeBar-Windows.git
cd KimiCodeBar-Windows
npm install

npm run tauri dev     # dev mode (hot reload)
npx tauri build       # NSIS installer + portable exe (src-tauri/target/release/)
```

Run tests:

```bash
cd src-tauri && cargo test
```

## Tech stack

| Layer | Choice |
|---|---|
| Framework | Tauri 2 (tray, frameless panel, notifications, autostart, opener, global shortcut) |
| Backend | Rust: reqwest (native-tls), serde, tokio, keyring (Windows Credential Manager), tracing |
| Frontend | React 18 + TypeScript + Vite, i18next (zh/en), no heavy UI libraries |
| Persistence | JSON files (settings / cache / history) + Windows Credential Manager (API key, web refresh token) + DPAPI-encrypted file (OAuth) |
| Packaging | NSIS (per-user install, no admin) + portable zip |

## Engineering & quality

- **CI gates**: every push / PR runs `cargo fmt --check`, `cargo clippy -D warnings`, ESLint and `cargo test` ([workflow](.github/workflows/ci.yml))
- **Test strategy**: 390+ unit/integration tests focusing on defensive parsing of the usage API (missing fields, proto3 omissions, field aliases, currency unit conversion), OAuth flow logic, version comparison and storage round-trips; real API responses (sanitized) are kept as regression fixtures
- **Dependabot**: weekly PRs for npm / cargo / GitHub Actions updates
- **Automated releases**: pushing a `v*` tag builds the NSIS installer and portable zip in CI and drafts a GitHub Release ([workflow](.github/workflows/release.yml))

## Contributing

Issues and PRs are welcome — please read [CONTRIBUTING.md](CONTRIBUTING.md) first. When reporting a bug, attach a diagnostics file from the app (Settings → Diagnostics & Logs).

## Acknowledgments

This project's behavior and API details (device flow, edge cases in usage parsing) were informed by:

- [xifandev/KimiCodeBar](https://github.com/xifandev/KimiCodeBar) — the original macOS app (MIT license) and the inspiration for this project; its [official site](https://xifandev.github.io/KimiCodeBar/) and repo description now point Windows users to this project
- [Golden0Voyager/kimi-code-usage](https://github.com/Golden0Voyager/kimi-code-usage) — Kimi Code usage CLI (Python), used as a reference
- [Javis603/token-monitor](https://github.com/Javis603/token-monitor) — reference for the web console's monthly usage API

## License

[MIT](LICENSE) © 2026 xifandev, JYH1878

This project has applied for free code signing from the SignPath Foundation (pending — releases stay unsigned until approved):
Free code signing provided by SignPath.io, certificate by SignPath Foundation
