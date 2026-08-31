# Lemmate — working notes for Claude

Lemmate is a self-hosted, open-source, multi-user markdown note app (an Obsidian replacement).
Crates are `lemmate-core`/`lemmate-server`/`lemmate-cli`/`lemmate-desktop`, binaries `lemmate` and
`lemmate-server`, env vars `LEMMATE_*`, vault sidecar `.lemmate/`, per-user config wherever
`lemmate_core::paths::config_dir()` points (`~/.config/lemmate` on this machine). The repository
directory is still called `notes`. The vault-doc CRDT map key `"notes"` (`NOTES_FIELD`) is wire
format and keeps its name.

`SPEC.md` is the design authority (decisions marked **[decided]** are settled — do not
re-litigate: CRDT is the truth and files are a projection; CodeMirror 6 live preview, not
WYSIWYG; Tauri 2 native shells; no P2P, no plugins, no E2E encryption). `README.md` has the
milestone status; `docs/guide.md` is the user guide; `docs/deploy.md` covers Docker/fly.io.

## Layout

| Path | What |
|---|---|
| `crates/core` | Everything shared: yrs CRDT docs (`doc.rs`, `vault_doc.rs`), SQLite store (`store.rs`), sync engine + local relay (`client.rs`, `local.rs`), projection/watcher, markdown indexer, attachments, TLS, credentials + per-platform config paths (`paths.rs`), import/export, pandoc |
| `crates/server` | axum server: WebSocket relay (`app.rs`), accounts/roles/shares (`auth.rs`), REST |
| `crates/cli` | `lemmate` binary: local commands, remote commands (`remote.rs`), MCP server (`mcp.rs`) |
| `crates/desktop` | Tauri 2 shell: starts the relay for the configured vault and opens one window on it |
| `ui/` | Svelte 5 + CodeMirror 6 client: `src/lib/` (`sync.ts` frame protocol, `vault.svelte.ts` one vault, `workspace.svelte.ts` all of them on one socket, `api.ts`, `import.ts`, `editor/`), `src/components/`, and `src/markdown/index.ts` — the TS indexer that must agree with the Rust one |
| `corpus/` | Markdown fixtures both indexers (Rust and TS) must agree on |

## Commands

```sh
cargo test --workspace --exclude lemmate-desktop        # Rust tests (desktop needs webkit2gtk; `cargo check -p lemmate-desktop`)
LEMMATE_TEST_PANDOC=/usr/bin/pandoc cargo test -p lemmate-core pandoc::    # skipped unless the var is set
cargo clippy --workspace --exclude lemmate-desktop --all-targets -- -D warnings && cargo fmt --all --check
cd ui && npm run check && npm test                     # svelte-check + tsc; corpus + codec tests
LEMMATE_SERVER_BIN=<target>/debug/lemmate-server LEMMATE_CLI_BIN=<target>/debug/lemmate npm test   # live e2e too
npm run build                                          # → ui/dist, served by `lemmate-server --web-dir ui/dist`
lemmate-server --no-auth --data-dir ./data --web-dir ui/dist   # dev server (auth off = dev only)
node ui/scripts/cdp.mjs <url> <outdir> 'waitfor:…' 'click:…' 'eval:…' 'shot:name'   # headless-Chrome smoke runs
```

CI (`.github/workflows/ci.yml`) runs the checks above on Linux, and three more jobs you cannot
reproduce here: `cargo check -p lemmate-desktop` with Tauri's system deps, a workspace check plus
the credentials tests on macOS and Windows, and a Docker image build. Keep the local ones green
before committing; the cross-platform legs mostly catch unix-only assumptions.

## Conventions

- Rust 1.95, edition 2024 (let-chains are fine), `rustfmt.toml` max_width 110, clippy clean under `-D warnings`.
- Every behaviour change gets a test at the right level: unit in the module, integration under
  `crates/*/tests` (they spin real servers/relays in-process), or the Node e2e suites.
- Server metadata (notes/tags/FTS rows) is *derived* from the CRDT stream — never a second
  source of truth. Writes through the REST API go through the room doc and are fanned out like
  client updates (`commit_change` in `app.rs`); on the relay they are file operations.
- The UI creates note text before the vault-doc entry; the server/engine tolerate that order.
- The client holds **every vault at once** over one WebSocket (frames are addressed by doc id):
  `Workspace` owns the socket and fans `onSynced`/`onDenied` out to its `VaultSession`s. Note
  ids are unique across vaults, so tabs, search hits and links still name a note by id alone.
- Obsidian import is Rust in both directions: `import::import_upload` classifies and converts
  one uploaded file, the server creates notes through the room docs, the relay writes them into
  the vault folder. The UI only batches the upload (`ui/src/lib/import.ts`).
- `apply_update` reports "changed" using state vector *and* delete set — deletions are changes.
- Commit messages: short imperative title, body explaining why. **No `Co-Authored-By: Claude` or
  `Claude-Session:` trailers** — Juan does not sign commits as Claude, and the history was
  rewritten on 2026-08-30 to remove the ones that were there.

## Environment (this machine)

- `.cargo/config.toml` is gitignored and machine-local: target dir is `/mnt/data/tmp/cargo-target-notes`
  (a quota-limited tmpfs; incremental off). The repo folder is Syncthing-synced — never let
  `target/` or `node_modules/` land inside it.
- `google-chrome-stable` is installed; there is no Chrome MCP/extension — use `ui/scripts/cdp.mjs`.
  `click:` does not focus inputs (use `eval:…focus()` before `type:`); reap stale Chrome with
  `pgrep -f "remote-debugging-por[t]"` patterns, never a `pkill -f` that can match your own shell.
- pandoc 3.10.1 (`/usr/bin/pandoc`) and quarto 1.10.18 (`~/.local/bin/quarto`) are installed, so
  export and render can be exercised here. The pandoc tests still gate on `LEMMATE_TEST_PANDOC`
  rather than on `PATH` — set it to `/usr/bin/pandoc` and all three run.
- The Android SDK is at `/opt/android-sdk`, but it holds only `cmdline-tools/latest`: no
  platforms, build-tools or NDK, and `ANDROID_HOME` is unset. Bootstrapped, not yet buildable —
  a Tauri Android target needs those installed first.
- Registry sources live under `~/.cargo/registry/src/*/` — check crate APIs there; versions have
  moved past training data (yrs 0.27 with built-in `sync`, ulid 3 `Ulid::generate()`, ureq 3,
  axum 0.8, notify 8, similar 3, tauri 2).

## Gotchas learned the hard way

- Scripted edits with exact-string anchors break after `cargo fmt` reflows lines; anchor on
  stable tokens or use whitespace-tolerant regexes, and check the script actually wrote.
- Python `re.sub` replacement strings turn `\"` into literal backslashes in Rust source.
- Rust raw strings: `r#"…"}"#…"#` terminates early on `"#` — use `r##"…"##`.
- Node's type stripping rejects TS parameter properties; `erasableSyntaxOnly` is on for a reason.
- CodeMirror block widgets must come from a `StateField`, not a `ViewPlugin`.
- `tauri-build` resolves `bundle.resources` (`../../ui/dist/`) in the build script, so *any*
  `cargo check`/`build`/`run` touching `lemmate-desktop` fails with "resource path … doesn't
  exist" unless `ui/dist` exists. The contents do not matter — `mkdir -p ui/dist` is enough, and
  that is what CI does. It passes here only because `npm run build` has left the directory behind.
- Env bool flags use `BoolishValueParser` (`1/0/yes/no` work); `LEMMATE_*` names are in `crates/server/src/main.rs`.
