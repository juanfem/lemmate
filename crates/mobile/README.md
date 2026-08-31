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

Nothing in this repository builds an APK yet, and the machine this was written on could not:
`cargo check -p lemmate-mobile` and `cargo test -p lemmate-mobile` work (both run in CI), but
the Android toolchain is a separate install. What is needed, once:

```sh
# 1. Rust std for the Android targets. This needs rustup — a distro Rust package will not do,
#    because it ships only the host target and has no way to add others.
rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android

# 2. The Tauri CLI.
cargo install tauri-cli --version '^2'

# 3. SDK packages, into wherever ANDROID_HOME points.
sdkmanager 'platforms;android-35' 'build-tools;35.0.0' 'ndk;27.2.12479018' 'platform-tools'

# 4. The environment the Tauri CLI reads.
export ANDROID_HOME=/opt/android-sdk
export NDK_HOME=$ANDROID_HOME/ndk/27.2.12479018
export JAVA_HOME=...   # a JDK the Android Gradle Plugin supports: 17 or 21, not 25

# 5. Generate the Gradle project, once, and commit it — gen/android is source, not build output.
cargo tauri android init
cargo tauri android dev      # onto a device or emulator
cargo tauri android build    # an APK/AAB
```

`ui/dist` must exist and be current before any of those: `include_dir!` compiles it into the
binary, so `npm run build` in `ui/` is a prerequisite, not an afterthought. An empty directory
compiles to an empty bundle and the app comes up blank — `web::unpack` logs an error saying so.

### The one edit `android init` does not make for you

The webview loads `http://127.0.0.1:<port>`, and Android has blocked cleartext HTTP by default
since Android 9. Add `gen/android/app/src/main/res/xml/network_security_config.xml`:

```xml
<?xml version="1.0" encoding="utf-8"?>
<network-security-config>
  <!-- The relay is this app talking to itself over loopback. Nothing else is permitted. -->
  <domain-config cleartextTrafficPermitted="true">
    <domain includeSubdomains="false">127.0.0.1</domain>
  </domain-config>
</network-security-config>
```

and reference it from the `<application>` element in
`gen/android/app/src/main/AndroidManifest.xml`:

```xml
<application android:networkSecurityConfig="@xml/network_security_config" ...>
```

Do **not** set `android:usesCleartextTraffic="true"` instead: that permits cleartext to every
host, including the sync server, which is exactly what should stay refused.

## Not done yet

- **Projection out of the sandbox** (SPEC §6.3). The vault is inside app-private storage, so
  nothing else on the phone can see the `.md` files. Android is meant to expose the folder
  through the Storage Access Framework and iOS through the Files app
  (`UIFileSharingEnabled`), reconciling on foreground rather than watching — background
  watching is not reliable on either.
- **Attachment pinning** (SPEC §7): keep attachments for notes opened in the last 30 days, LRU
  beyond a configurable cache size, rather than the desktop's keep-everything.
- **The keyboard toolbar** (SPEC §8): a row above the on-screen keyboard for markup, indent,
  checkbox, link and image.
- **iOS** in general: it needs a macOS machine, and none of the above has been run there.
- Errors during startup only reach the log. On a desktop they go to stderr; a phone has no
  stderr to read, so a failure to start currently shows an empty webview.
