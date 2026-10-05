//! The `PATH` the shell hands to Quarto and pandoc lookups.
//!
//! An app started from the Finder, the Dock or a desktop launcher does not inherit the user's
//! shell `PATH`: on macOS launchd gives it `/usr/bin:/bin:/usr/sbin:/sbin`, which has neither
//! Quarto (`/usr/local/bin`, `/Applications/quarto/bin`) nor Homebrew's pandoc
//! (`/opt/homebrew/bin`). The relay then reported "no Quarto here" on a machine that has it.
//! So before anything else starts, the shell asks the user's login shell for its `PATH` and
//! appends that, and the usual install locations, to its own.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Brackets the `PATH` in the shell's output, which an interactive rc file may also write to.
const MARK: &str = "__LEMMATE_PATH__";
/// An rc file that hangs must not keep the app from starting.
const SHELL_TIMEOUT: Duration = Duration::from_secs(3);

/// Where Quarto, pandoc and a TeX for pandoc's PDFs install themselves, beyond the system dirs.
fn fallback_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/Applications/quarto/bin",
        "/Applications/RStudio.app/Contents/Resources/app/quarto/bin",
        "/Library/TeX/texbin",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    if let Some(home) = home {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join("bin"));
    }
    dirs
}

/// `current`, then whatever `login` and `fallbacks` add to it, without repeats. Fallback
/// directories are only added when `exists` says they are there.
fn widened(
    current: Option<&OsStr>,
    login: Option<&OsStr>,
    fallbacks: &[PathBuf],
    exists: impl Fn(&Path) -> bool,
) -> Option<OsString> {
    let mut dirs: Vec<PathBuf> = current.map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
    let original = dirs.len();
    let mut add = |d: PathBuf| {
        if !d.as_os_str().is_empty() && !dirs.contains(&d) {
            dirs.push(d);
        }
    };
    for d in login.map(|p| std::env::split_paths(p).collect::<Vec<_>>()).unwrap_or_default() {
        add(d);
    }
    for d in fallbacks.iter().filter(|d| exists(d)) {
        add(d.clone());
    }
    (dirs.len() > original).then(|| std::env::join_paths(dirs).ok()).flatten()
}

/// The `PATH` an interactive login shell of the user's sets up, if it says within the timeout.
fn login_shell_path(shell: &OsStr) -> Option<OsString> {
    let mut child = Command::new(shell)
        .args(["-ilc", &format!("printf '%s' \"{MARK}${{PATH}}{MARK}\"")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < SHELL_TIMEOUT => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = std::io::read_to_string(child.stdout.take()?).ok()?;
    parse_marked(&out).map(OsString::from)
}

fn parse_marked(out: &str) -> Option<&str> {
    let (_, rest) = out.split_once(MARK)?;
    let (path, _) = rest.split_once(MARK)?;
    (!path.is_empty()).then_some(path)
}

/// Widen this process's `PATH`. Must run before any other thread starts: `set_var` is not
/// safe alongside a thread reading the environment.
pub fn widen() {
    if cfg!(windows) {
        // Windows hands GUI apps the system and user PATH from the registry already.
        return;
    }
    let current = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let login = std::env::var_os("SHELL").filter(|s| !s.is_empty()).and_then(|s| login_shell_path(&s));
    if let Some(path) =
        widened(current.as_deref(), login.as_deref(), &fallback_dirs(home.as_deref()), Path::is_dir)
    {
        // SAFETY: called first thing in `main`, before the tracing subscriber, Tauri or the
        // relay have started a thread.
        unsafe { std::env::set_var("PATH", path) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_login_and_existing_fallbacks_without_repeats() {
        let fallbacks = [PathBuf::from("/usr/local/bin"), PathBuf::from("/Applications/quarto/bin")];
        let path = widened(
            Some(OsStr::new("/usr/bin:/bin")),
            Some(OsStr::new("/opt/homebrew/bin:/usr/bin")),
            &fallbacks,
            |d| d != Path::new("/Applications/quarto/bin"),
        )
        .unwrap();
        assert_eq!(path, "/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin");
    }

    #[test]
    fn leaves_a_complete_path_alone() {
        let fallbacks = [PathBuf::from("/usr/local/bin")];
        let current = OsStr::new("/usr/local/bin:/usr/bin");
        assert_eq!(widened(Some(current), Some(OsStr::new("/usr/bin")), &fallbacks, |_| true), None);
        assert_eq!(
            widened(None, None, &fallbacks, |_| true).unwrap(),
            "/usr/local/bin",
            "no PATH at all still gets the fallbacks"
        );
    }

    #[cfg(unix)]
    #[test]
    fn asks_a_real_shell() {
        let path = login_shell_path(OsStr::new("/bin/sh")).expect("sh reports a PATH");
        assert!(!path.is_empty() && !path.to_string_lossy().contains(MARK), "{path:?}");
    }

    #[test]
    fn reads_the_path_between_the_marks() {
        let out = format!("motd noise\n{MARK}/a:/b{MARK}");
        assert_eq!(parse_marked(&out), Some("/a:/b"));
        assert_eq!(parse_marked("no marks"), None);
        assert_eq!(parse_marked(&format!("{MARK}{MARK}")), None);
    }
}
