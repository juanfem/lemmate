//! Where per-user files live. Every native client (CLI, desktop) resolves its configuration
//! through here, so the platform conventions are decided in exactly one place.

use std::path::PathBuf;

/// Lemmate's per-user configuration directory:
///
/// | Platform | Location |
/// |---|---|
/// | Linux / BSD | `$XDG_CONFIG_HOME/lemmate`, else `~/.config/lemmate` |
/// | macOS | `~/Library/Application Support/lemmate` |
/// | Windows | `%APPDATA%\lemmate` |
///
/// `$LEMMATE_CONFIG_DIR` overrides all of them (it is the directory itself, not a parent), which
/// is how tests get an isolated one on every platform. `None` when the platform cannot say where
/// the user's home is — on Windows that means `%APPDATA%` is unset, which is close to impossible.
pub fn config_dir() -> Option<PathBuf> {
    match std::env::var_os("LEMMATE_CONFIG_DIR") {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => Some(dirs::config_dir()?.join("lemmate")),
    }
}

/// The user's home directory, used only to *suggest* paths (the setup screen's default vault
/// folder). Never used to build a configuration path — that is `config_dir`'s job.
pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}
