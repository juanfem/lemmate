//! Where per-user files live. Every native client (CLI, desktop) resolves its configuration
//! through here, so the platform conventions are decided in exactly one place.

use std::path::{Path, PathBuf};

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

/// Where native clients write their log files:
///
/// | Platform | Location |
/// |---|---|
/// | Linux / BSD | `$XDG_STATE_HOME/lemmate`, else `~/.local/state/lemmate` |
/// | macOS | `~/Library/Logs/lemmate` |
/// | Windows | `%LOCALAPPDATA%\lemmate\logs` |
///
/// Under `$LEMMATE_CONFIG_DIR` when that is set, so a test or a portable setup keeps everything
/// in one place.
pub fn log_dir() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("LEMMATE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(v).join("logs"));
    }
    if cfg!(target_os = "macos") {
        return Some(dirs::home_dir()?.join("Library").join("Logs").join("lemmate"));
    }
    if cfg!(windows) {
        return Some(dirs::data_local_dir()?.join("lemmate").join("logs"));
    }
    Some(dirs::state_dir().or_else(dirs::data_local_dir)?.join("lemmate"))
}

/// The user's home directory, used only to *suggest* paths (the setup screen's default vault
/// folder). Never used to build a configuration path — that is `config_dir`'s job.
pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Create `dir` and its parents; on unix the directories this creates are `0700`, since the
/// configuration directory holds tokens. An existing directory keeps its mode.
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)
}

/// Replace `path` with `contents` atomically, readable by its owner only: the bytes go into a
/// fresh temporary file beside it (created `0600` on unix, so there is no moment when a token sits
/// in a world-readable file), are flushed to disk, and the file is renamed over `path`. A crash
/// leaves either the old file or the new one, never half of one.
pub fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    create_private_dir(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).map_err(|e| std::io::Error::other(e.to_string()))?;
    let nonce: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let tmp = dir.join(format!(".{name}.{nonce}.tmp"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_writes_replace_the_file_and_leave_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conf").join("secret.toml");
        write_private(&path, b"one").unwrap();
        write_private(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap()).collect();
        assert_eq!(names.len(), 1, "no temporary file left over");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
        }
    }
}
