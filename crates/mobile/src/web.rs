//! The web client, compiled into the binary and unpacked on first run.
//!
//! The desktop ships `ui/dist` through `bundle.resources` and points the relay's `web_dir` at
//! the copy Tauri installed. That does not carry over: on Android the bundle lands inside the
//! APK, where entries are compressed stream members rather than files a `ServeDir` can open,
//! and on iOS the resource bundle is read-only. Embedding the tree and writing it out once
//! into the app's own data directory keeps the relay's file-serving path exactly as it is on
//! every other platform, at the cost of carrying the assets twice in the install.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use include_dir::{Dir, include_dir};

/// The built UI. Compiled in, so a release binary is self-contained.
///
/// This is why `ui/dist` has to exist before the crate builds — the same requirement the
/// desktop crate imposes through `bundle.resources`, and the reason CI runs `mkdir -p
/// ui/dist` before it type-checks either of them. An empty directory compiles to an empty
/// bundle and the app would come up blank, so [`unpack`] says so out loud.
static WEB: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../ui/dist");

/// Written beside the unpacked tree so an upgrade replaces stale assets, and an unchanged
/// install does not rewrite a thousand files on every launch.
const STAMP: &str = ".lemmate-web-stamp";

/// Unpack the bundled client into `dir` if what is there is not already this build, and
/// return the directory to hand the relay.
pub fn unpack(dir: &Path) -> Result<PathBuf> {
    let stamp = stamp();
    let stamp_path = dir.join(STAMP);
    if std::fs::read_to_string(&stamp_path).is_ok_and(|s| s == stamp) {
        tracing::debug!(dir = %dir.display(), "web client already unpacked");
        return Ok(dir.to_path_buf());
    }

    if WEB.entries().is_empty() {
        // Not fatal: the shell still starts, and saying which build is empty beats a blank
        // screen with nothing in the log.
        tracing::error!("the bundled web client is empty — was `npm run build` run in ui/?");
    }

    // Replace rather than merge: a file dropped between releases must not survive.
    if dir.exists() {
        std::fs::remove_dir_all(dir).with_context(|| format!("clearing {}", dir.display()))?;
    }
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    WEB.extract(dir).with_context(|| format!("extracting the web client into {}", dir.display()))?;
    std::fs::write(&stamp_path, &stamp).with_context(|| format!("writing {}", stamp_path.display()))?;
    tracing::info!(dir = %dir.display(), files = count(&WEB), "unpacked the web client");
    Ok(dir.to_path_buf())
}

/// What counts as "the same build": the crate version plus the shape of the tree. The hashed
/// file names Vite emits mean a changed bundle changes this, without hashing the bytes.
fn stamp() -> String {
    let mut names: Vec<&str> = Vec::new();
    collect(&WEB, &mut names);
    names.sort_unstable();
    format!("{} {} {}", env!("CARGO_PKG_VERSION"), names.len(), names.join(","))
}

fn collect<'a>(dir: &'a Dir<'a>, out: &mut Vec<&'a str>) {
    for file in dir.files() {
        out.push(file.path().to_str().unwrap_or("?"));
    }
    for sub in dir.dirs() {
        collect(sub, out);
    }
}

fn count(dir: &Dir<'_>) -> usize {
    dir.files().count() + dir.dirs().map(count).sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacking_is_idempotent_and_replaces_a_stale_tree() {
        let dir = std::env::temp_dir().join(format!("lemmate-web-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();

        unpack(&dir).unwrap();
        let stamp_path = dir.join(STAMP);
        assert_eq!(std::fs::read_to_string(&stamp_path).unwrap(), stamp());

        // A leftover from an older build is gone after a re-unpack with a different stamp.
        let stale = dir.join("assets").join("index-OLDHASH.js");
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(&stale, b"old").unwrap();
        std::fs::write(&stamp_path, "not-this-build").unwrap();
        unpack(&dir).unwrap();
        assert!(!stale.exists(), "a file from an older bundle survived the upgrade");

        // And an unchanged install is left alone: the marker below is only removed by a
        // re-extract, which the matching stamp must prevent.
        let marker = dir.join("untouched-marker");
        std::fs::write(&marker, b"x").unwrap();
        unpack(&dir).unwrap();
        assert!(marker.exists(), "an unchanged bundle was needlessly re-extracted");

        std::fs::remove_dir_all(&dir).ok();
    }
}
