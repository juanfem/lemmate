# Installing Lemmate

Nothing is signed, and nothing is in a distribution's repositories. CI does package plain
builds — see [Ready-made builds](#ready-made-builds) below — and everything else in this
document builds from source. Three things can be installed, and few people need all three.

| What | Binary | When you need it |
|---|---|---|
| Server | `lemmate-server` | One machine that other devices sync with. On Linux, [Docker](deploy.md) is usually easier than building. |
| CLI | `lemmate` | Sync a folder headlessly, or drive a server from a terminal, an editor, or MCP. |
| Desktop app | `Lemmate` (`lemmate-desktop`) | A window over a local relay: the offline-capable client. |

The CLI alone is enough to use a Lemmate server — `lemmate sync --vault ~/vault --serve
127.0.0.1:8081 --web-dir …` gives you the same UI in a browser tab, without a webview.

## Ready-made builds

Every push to `main`, and every `v*` tag, runs the `Artifacts` job in
[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) on Linux, macOS and Windows. Each one
uploads, to that workflow run:

- `lemmate-<platform>.tar.gz` (`.zip` on Windows) — the `lemmate` and `lemmate-server` binaries,
  the web client in `web/` for `--web-dir`, and these docs.
- the desktop installers for that platform: `.deb`, `.rpm` and an AppImage on Linux, a `.dmg` on
  macOS, an `.msi` and an NSIS `-setup.exe` on Windows.

Download them from the run's page under **Artifacts**; a `v*` tag also collects all three sets
into a draft GitHub release. They are kept for 14 days, and there is no Android APK — an
unsigned one would not install on any device.

None of it is signed or notarised, so the desktop installers need the same hand past macOS's
Gatekeeper and Windows' SmartScreen as a bundle you built yourself; both are described below.
Binaries are built for the runner's architecture — x86-64 on Linux and Windows, Apple silicon on
macOS — so anything else still means building from source.

## Prerequisites everywhere

- **Rust 1.95 or newer**, from [rustup](https://rustup.rs). `rustup update stable` if you have it
  already; `rustc --version` to check.
- **Node 24 or newer**, but only to build the web client (`ui/dist`). The server serves it with
  `--web-dir`, the local relay and the desktop bundle need it too. A CLI-only install can skip Node.
- **git**, to get the source.
- Optional: **pandoc** ≥ 3.1 for the server's note-export endpoint (§7 of the
  [guide](guide.md)), plus a LaTeX engine for PDF and Beamer; **quarto** for `.qmd`. `lemmate
  doctor` reports whether it found them. `lemmate export zip` never needs either.

Everything else is vendored: SQLite is compiled in and TLS is rustls, so there is no OpenSSL and
no system SQLite to install — a C toolchain for the platform is all the native side needs. The
webview is the system one on macOS and Windows; only Linux needs development packages for it.

## Building the server and the CLI

Identical on all three platforms, from a checkout of the repository:

```sh
cd ui && npm ci && npm run build && cd ..     # → ui/dist; skip for a CLI-only install
cargo build --release --workspace --exclude lemmate-desktop
```

The binaries land in `target/release/` (`lemmate`, `lemmate-server`, plus `.exe` on Windows). To
put them on `PATH` instead, `cargo install` them — rustup already puts its bin directory
(`~/.cargo/bin`, `%USERPROFILE%\.cargo\bin`) on `PATH`:

```sh
cargo install --path crates/cli        # lemmate
cargo install --path crates/server     # lemmate-server
```

`ui/dist` is *not* installed with the server: keep the checkout, or copy the folder somewhere
stable and point `--web-dir` (`LEMMATE_WEB_DIR`) at it. See [`deploy.md`](deploy.md) for running
the server for real — Docker, a reverse proxy, fly.io, backups.

## Building the desktop app

The Tauri shell needs the web assets to exist before it compiles at all (`tauri-build` resolves
`bundle.resources`), so build `ui/dist` first. To run it straight from the source tree:

```sh
cargo run -p lemmate-desktop            # reads desktop.toml, or opens the setup screen
```

To produce an installer, use the Tauri CLI from `crates/desktop`; the bundles appear under
`target/release/bundle/`:

```sh
cargo install tauri-cli --version '^2'
cd crates/desktop && cargo tauri build
```

Per-platform prerequisites and bundle formats are below.

---

## Linux

System packages for the desktop shell (the server and CLI need only a C toolchain):

```sh
# Debian / Ubuntu
sudo apt install build-essential curl wget file libwebkit2gtk-4.1-dev librsvg2-dev libxdo-dev

# Fedora
sudo dnf install @development-tools curl wget file webkit2gtk4.1-devel librsvg2-devel libxdo-devel

# Arch
sudo pacman -S base-devel curl wget file webkit2gtk-4.1 librsvg xdotool
```

Tray icons are off (`tauri`'s `tray-icon` feature is disabled), so libayatana-appindicator is
*not* required even though Tauri's own instructions list it.

`cargo tauri build` writes `.deb`, `.rpm` and an AppImage under
`target/release/bundle/{deb,rpm,appimage}/`. If the AppImage step stops at `failed to run
linuxdeploy`, the tools it downloads are AppImages themselves and your distribution has only
FUSE 3; build it as CI does, with `APPIMAGE_EXTRACT_AND_RUN=1 NO_STRIP=1 cargo tauri build`
(`NO_STRIP` because linuxdeploy's strip pass fails on a binary the release profile has already
stripped). The `.deb` and `.rpm` need neither. Per-user files live in `$XDG_CONFIG_HOME/lemmate`,
falling back to `~/.config/lemmate`.

## macOS

```sh
xcode-select --install                       # clang, headers, linker
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
brew install node                            # must be ≥ 24
brew install pandoc                          # optional, for export
```

WKWebView is part of the system; there is nothing else to install for the desktop shell. Apple
silicon and Intel both build natively; for a universal binary add both targets first:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cd crates/desktop && cargo tauri build --target universal-apple-darwin
```

`cargo tauri build` writes `Lemmate.app` under `target/release/bundle/macos/` and a `.dmg` next
to it in `bundle/dmg/`. The app is **unsigned and unnotarised**, so Gatekeeper refuses it on
first launch: open it once from the context menu (right-click → Open → Open), or clear the
quarantine flag after copying it into `/Applications`:

```sh
xattr -dr com.apple.quarantine /Applications/Lemmate.app
```

Signing it properly needs an Apple Developer ID and the `APPLE_*` environment variables Tauri's
signing docs describe. Per-user files live in `~/Library/Application Support/lemmate`.

## Windows

Install, in this order:

1. **Microsoft C++ Build Tools** — the [Visual Studio 2022 Build
   Tools](https://visualstudio.microsoft.com/downloads/) with the *Desktop development with C++*
   workload (a full Visual Studio install works too). Rust's default toolchain links with MSVC.
2. **Rust** — `rustup-init.exe` from [rustup.rs](https://rustup.rs), default host
   `x86_64-pc-windows-msvc` (or `aarch64-pc-windows-msvc` on Arm).
3. **Node 24+** — from [nodejs.org](https://nodejs.org) or `winget install OpenJS.NodeJS.LTS`.
4. **WebView2 runtime** — only for the desktop app, and already present on Windows 11 and
   up-to-date Windows 10; otherwise install the Evergreen Bootstrapper from Microsoft.
5. Optional: `winget install JohnMacFarlane.Pandoc` for export.

Then, in PowerShell:

```powershell
cd ui; npm ci; npm run build; cd ..
cargo build --release --workspace --exclude lemmate-desktop
.\target\release\lemmate.exe doctor
```

`cargo tauri build` writes an MSI under `target\release\bundle\msi\` and an NSIS
`…-setup.exe` under `bundle\nsis\`; the WiX and NSIS toolchains are downloaded on first use.
Neither is code-signed, so SmartScreen shows "Windows protected your PC" on first run: *More
info* → *Run anyway*, or unblock the file in its Properties dialog. Signing it properly needs a
code-signing certificate and Tauri's `windows.certificateThumbprint` config.

Per-user files live in `%APPDATA%\lemmate`. `credentials.toml` holds session tokens and is
chmod 0600 on Unix — on Windows it simply inherits your profile's ACL, so treat a shared
machine account as a shared token.

Shell notes for the examples elsewhere in these docs: they are POSIX shell. `VAR=x cmd` becomes
`$env:VAR='x'; cmd`, `~` becomes `$HOME`, and single quotes around `--filter` arguments are not
optional in PowerShell either. `lemmate edit` opens `$env:VISUAL`/`$env:EDITOR`, defaulting to
`vi`, which Windows does not ship — set one, e.g. `$env:EDITOR='notepad'`.

---

## After installing

```sh
lemmate doctor                                                    # version, schema, sqlite, pandoc
lemmate login --server https://notes.example.org --email you@example.org --register
lemmate sync  --vault ~/vault --server https://notes.example.org
```

The [user guide](guide.md) picks up from here: §1 for first runs of the server, desktop app and
CLI, §9 for where every file lives and how to reset a device.
