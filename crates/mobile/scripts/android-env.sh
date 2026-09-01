# Source this before `cargo tauri android …`:  . crates/mobile/scripts/android-env.sh
#
# Three variables and one PATH order, all of which the Tauri CLI reads rather than discovers.
# The paths are the ones this project was set up with; override any of them in your environment
# before sourcing, and they are left alone.

# rustup's cargo must come first: it is the one that has the Android std targets. A distro Rust
# package ships only the host target, and `cargo tauri android build` fails on the first
# `--target aarch64-linux-android` with nothing more helpful than "can't find crate for `std`".
[ -d "$HOME/.cargo/bin" ] && case ":$PATH:" in
  *":$HOME/.cargo/bin:"*) ;;
  *) PATH="$HOME/.cargo/bin:$PATH"; export PATH ;;
esac

export ANDROID_HOME="${ANDROID_HOME:-/opt/android-sdk}"

# Tauri wants a specific NDK, not the directory holding several. Take the newest installed one
# unless the caller has already picked.
if [ -z "${NDK_HOME:-}" ] && [ -d "$ANDROID_HOME/ndk" ]; then
  NDK_HOME="$ANDROID_HOME/ndk/$(ls -1 "$ANDROID_HOME/ndk" | sort -V | tail -1)"
  export NDK_HOME
fi

# The Android Gradle Plugin refuses a JDK newer than it knows about, so this is deliberately not
# whatever `java` happens to be on PATH: 25 is rejected even though sdkmanager accepts it.
if [ -z "${JAVA_HOME:-}" ]; then
  for candidate in /usr/lib/jvm/openjdk-bin-21 /usr/lib/jvm/java-21-openjdk /usr/lib/jvm/openjdk-bin-17 /usr/lib/jvm/java-17-openjdk; do
    [ -x "$candidate/bin/javac" ] && { JAVA_HOME="$candidate"; export JAVA_HOME; break; }
  done
fi

echo "ANDROID_HOME=$ANDROID_HOME"
echo "NDK_HOME=${NDK_HOME:-<none found>}"
echo "JAVA_HOME=${JAVA_HOME:-<none found — install a JDK 17 or 21>}"
