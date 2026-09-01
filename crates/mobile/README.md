# lemmate-mobile

The Tauri 2 shell for Android and iOS (SPEC §3.1, §14 — milestone M3).

It is the same shape as [`lemmate-desktop`](../desktop/README.md): the engine's local relay
runs in-process, the webview is pointed at `http://127.0.0.1:<port>`, and the relay serves the
shared TypeScript UI. Nothing crosses Tauri IPC, and the bundle is byte-identical to the one
`lemmate-server` serves on the web — which is the whole reason SPEC §3.1 picked a small
framework in the first place.

Three things the desktop gets for free and this crate has to arrange:

| | Desktop | Here |
|---|---|---|
| Vault folder | the user picks it | `<app data>/vault`; the setup form's folder field is ignored |
| Web assets | `bundle.resources` → real files on disk | compiled in with `include_dir!`, unpacked into `<app data>/web` on first run |
| Relay port | derived from the vault path | the same, and it matters more — see below |

**Why the port is derived rather than ephemeral.** The webview's origin *is* the relay's
address, and a browser partitions `localStorage` by origin. Binding port 0 would hand the UI a
new origin on every launch, silently discarding the open tabs and panes, pinned tabs, sidebar
width and folder folds it keeps there — on a phone, where the OS kills and relaunches the app
constantly, that is every few minutes. `lemmate_core::local::stable_port` derives a port in the
dynamic range from the vault path; if something already holds it the shell falls back to an
ephemeral one, because a forgotten layout beats an app that will not open.

**Why the assets are compiled in.** On the desktop the relay reads `ui/dist` off disk from
wherever Tauri installed it. That does not carry over: an APK's resources are compressed
members of a zip rather than files a `ServeDir` can open, and an iOS resource bundle is
read-only. Embedding the tree and writing it out once keeps the relay's file-serving path
identical on every platform. The cost is carrying the assets twice in the install; the
alternative — serving the UI from Tauri's own asset protocol — would mean teaching `api.ts` and
`sync.ts` a base URL, plus CORS on the relay, to save a megabyte.

## Building it

`cargo check -p lemmate-mobile` and `cargo test -p lemmate-mobile` are what CI runs, and they
need nothing beyond Tauri's usual Linux packages. An APK needs the Android toolchain as well.

On the machine this was written on that toolchain is installed, and it gets as far as a real
library: `cargo build -p lemmate-mobile --lib --release --target aarch64-linux-android` produces
a 20 MB stripped `liblemmate_mobile.so` — ELF aarch64, built for Android 24 against NDK r27d,
linked only against `libandroid`/`libdl`/`liblog`/`libm`/`libc`, and exporting the JNI entry
points Tauri's Android glue calls (`Java_dev_lemmate_mobile_Rust_create` and friends, named
after `identifier` in tauri.conf.json). Everything `lemmate-core` pulls in cross-compiles,
rusqlite's bundled SQLite and ring included.

`cargo tauri android build --apk --target aarch64` then produces a 23 MB unsigned release APK
containing that library at `lib/arm64-v8a/`. It has not been installed or run on a device: an
APK that assembles proves the toolchain and the manifest, not that the app works.

Build Android targets with `--release`. A debug `staticlib` bundles every rlib in the graph and
is large enough to exhaust a quota-limited target directory outright — the failure reads
`Disk quota exceeded (os error 122)` from `llvm-ar`, which looks like a toolchain fault and is
not one.

The steps, once:

```sh
# 1. Rust std for the Android targets. This needs rustup — a distro Rust package ships only the
#    host target and has no way to add others, which is the one genuinely blocking prerequisite.
rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android

# 2. The Tauri CLI.
cargo install tauri-cli --version '^2' --locked

# 3. SDK packages, into wherever ANDROID_HOME points. The NDK is ~3 GB.
sdkmanager 'platform-tools' 'platforms;android-35' 'platforms;android-36' \
           'build-tools;35.0.1' 'build-tools;36.1.0' 'ndk;27.3.13750724'

# 4. A JDK the Android Gradle Plugin supports — 17 or 21. Newer ones (25) are rejected by AGP
#    even though sdkmanager itself is happy with them, so this often means a second JDK
#    alongside the system one rather than replacing it.

# 5. Generate the Gradle project, once, and commit it — gen/android is source, not build output.
cargo tauri android init
cargo tauri android dev      # onto a device or emulator
cargo tauri android build    # an APK/AAB
```

`scripts/android-env.sh` in this directory sets the three variables the CLI reads
(`ANDROID_HOME`, `NDK_HOME`, `JAVA_HOME`) and puts rustup's cargo ahead of a distro one on
`PATH`; `source` it before any `cargo tauri android …`. Its paths are this machine's — edit
them, or override the variables, on any other.

`ui/dist` must exist and be current before any of those: `include_dir!` compiles it into the
binary, so `npm run build` in `ui/` is a prerequisite, not an afterthought. An empty directory
compiles to an empty bundle and the app comes up blank — `web::unpack` logs an error saying so.

### The edit `android init` does not make for you

`gen/android` is generated but committed, and it carries one deliberate change. Tauri's template
drives `android:usesCleartextTraffic` from a Gradle manifest placeholder — `false` in release,
`true` in debug — and **both settings are wrong here**. The webview loads
`http://127.0.0.1:<port>`, and Android has refused cleartext by default since Android 9, so
`false` would leave a release build unable to load anything at all; `true` would permit cleartext
to every host, the sync server included, which is exactly what must stay refused.

So the placeholder is gone from the manifest and from `app/build.gradle.kts`, replaced by
`res/xml/network_security_config.xml`, which permits cleartext to `127.0.0.1` and nothing else,
in every build type. Verified in the assembled release APK rather than assumed —
`aapt2 dump xmltree` on it decodes to exactly that one domain, and the manifest carries
`networkSecurityConfig` with no `usesCleartextTraffic` anywhere.

**Re-running `cargo tauri android init` regenerates both files and will undo this.** If you ever
have to, re-apply it from the committed version.

### Two things that will bite

`build-tools;35.0.0` is needed on top of anything newer you have installed: AGP's R8 step asks
for that exact version, and if the SDK directory is not writable it fails with `Failed to install
the following SDK components` rather than saying which build wanted what.

Gradle's build directory lands in `gen/android/app/build` — 130 MB or so after one APK. It is
gitignored, but this repository is Syncthing-synced, so it is worth an ignore pattern there for
the same reason `target/` and `node_modules/` are kept out of the tree.

## iOS, on a Mac

Nothing here can build for iOS: `cargo tauri ios init` shells out to `xcodebuild`, so it needs
macOS and Xcode, and there is no cross-compilation path. The crate itself is already shaped for
it — `staticlib` is in the crate-type list because that is what an iOS project links, and
`run()` is behind `tauri::mobile_entry_point` the same way — but not one line of that has been
compiled, let alone run.

On the Mac, the same prerequisites as Android with the platform swapped:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
cargo install tauri-cli --version '^2' --locked
xcode-select --install                    # or a full Xcode from the App Store
cd ui && npm install && npm run build     # include_dir! compiles ui/dist in; it must exist
cd ../crates/mobile && cargo tauri ios init
cargo tauri ios dev
```

`gen/apple` is source in the same way `gen/android` is: generated once, then committed and
edited. Signing needs an Apple ID — a free one installs to your own device for seven days at a
time, which is enough to find out whether any of this works.

### The thing to suspect first

The webview's whole content comes from `http://127.0.0.1:<port>`, and iOS App Transport
Security is hostile to cleartext HTTP in a way that took a deliberate exception to solve on
Android (see above). ATS's treatment of *loopback* specifically is less clear-cut than
Android's blanket ban — reports differ on whether `127.0.0.1` needs an exception at all, and it
has changed across iOS versions — so this is written down as a first suspect rather than a step.

If the app launches to a blank webview with nothing useful in the console, add to
`gen/apple/lemmate-mobile_iOS/Info.plist`:

```xml
<key>NSAppTransportSecurity</key>
<dict>
  <key>NSAllowsLocalNetworking</key>
  <true/>
</dict>
```

`NSAllowsLocalNetworking` permits local and loopback connections while leaving ATS in force for
everything else — the same scoping as the Android network-security-config, and for the same
reason: the sync server must keep being held to https. Do **not** reach for
`NSAllowsArbitraryLoads`.

### Two more that will come up

The vault lands in the app's private container and stays there: SPEC §6.3 no longer asks mobile
to project files at all. `UIFileSharingEnabled` and `LSSupportsOpeningDocumentsInPlace` are what
this *would* have needed, and are noted only so nobody adds them by reflex.

Xcode's build output lands under `gen/apple`, which is inside a Syncthing-synced tree on the
machine this was written on. `gen/android/app/build` needed an ignore pattern for exactly that
reason; expect to want one here too.

## Icons

One source at the repository root feeds everything: `icon.png` (1024², RGBA) for the desktop
sets and the web favicon, and `icon-android-foreground.png` for Android's adaptive icon.
Regenerate with

```sh
cargo tauri icon ../../icon.json          # from crates/mobile — writes gen/android's mipmaps
cd ../desktop && cargo tauri icon ../../icon.png
```

`icon.json` exists because the source is a finished tile — a cream rounded square with the mark
inside — and Android does not want one of those. An adaptive icon is a foreground drawn over a
background, and the launcher masks the pair to whatever shape it likes, keeping only the inner
72 of 108dp for certain. Handing it the tile whole meant the corners were cropped and what was
left sat on a white background it did not match.

So `bg_color` is the tile's own cream, `#FEF8EE`, which makes the cropping invisible: what gets
cut is cream against cream. The foreground is then a separate file rather than the source,
because `android_fg_scale` is only read when `android_fg` is given — it is the tile at 78%,
built by the snippet in the commit that added it, which puts the hexagon at 56% of the canvas
and so inside the 66dp circle Android asks you to keep key content within.

If you replace the artwork, the number to re-check is that last one, and the cream has to match
the new tile or the seam reappears.

## Not done yet

- ~~Projection out of the sandbox~~ — **dropped**, see SPEC §6.3. Files exist so other tools
  can reach the notes, and that is a desktop workflow.
- **Attachment pinning** (SPEC §7): keep attachments for notes opened in the last 30 days, LRU
  beyond a configurable cache size, rather than the desktop's keep-everything.
- **The keyboard toolbar** (SPEC §8): a row above the on-screen keyboard for markup, indent,
  checkbox, link and image.
- **iOS** in general — see the section above for what it needs and what to suspect first.
- Errors during startup only reach the log. On a desktop they go to stderr; a phone has no
  stderr to read, so a failure to start currently shows an empty webview.
