//! Where a download goes: the reader says, as a browser set to ask would.
//!
//! The page saves what it makes from a render — a deck's PDF or PowerPoint file, a Quarto render
//! to Word — as an ordinary download (`<a download>`), and a webview would put that straight into
//! the Downloads folder under the name the page suggested. Instead each download lands in a
//! folder of its own under the temp dir, and once it is there a save dialog asks where it goes,
//! starting in Downloads with that name: chosen, the file moves there; cancelled, it is deleted.
//! Asking after rather than before keeps the dialog out of the webview's callback, which has to
//! answer at once — and macOS reports no path when a download ends, so the staged one is kept
//! here, by URL.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tauri::webview::DownloadEvent;
use tauri::{Manager, Url, Webview};

/// Downloads under way: the URL asked for, and where it is being written.
static STAGED: Mutex<Vec<(Url, PathBuf)>> = Mutex::new(Vec::new());

/// The handler for a relay window's downloads (`WebviewWindowBuilder::on_download`).
pub fn on_download(webview: Webview, event: DownloadEvent<'_>) -> bool {
    match event {
        DownloadEvent::Requested { url, destination } => {
            let name = destination.file_name().map_or_else(|| "download".into(), |n| n.to_owned());
            match stage(&std::env::temp_dir(), Path::new(&name)) {
                Ok(path) => {
                    *destination = path.clone();
                    STAGED.lock().unwrap_or_else(|e| e.into_inner()).push((url, path));
                }
                // Then it goes where the webview meant to put it.
                Err(e) => tracing::warn!(error = %e, "could not stage a download"),
            }
        }
        DownloadEvent::Finished { url, success, .. } => {
            let staged = {
                let mut staged = STAGED.lock().unwrap_or_else(|e| e.into_inner());
                staged.iter().position(|(u, _)| *u == url).map(|i| staged.remove(i).1)
            };
            match staged {
                Some(path) if success => ask(&webview, path),
                Some(path) => discard(&path),
                None => {}
            }
        }
        _ => {}
    }
    true
}

/// A fresh path for `name` under `temp`: a folder of its own, so the name stays the suggested one.
fn stage(temp: &Path, name: &Path) -> std::io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let dir = temp.join("lemmate-downloads").join(format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(name))
}

/// Removes a staged file and the folder it was staged in.
fn discard(staged: &Path) {
    if let Some(dir) = staged.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Moves a staged file to where the reader chose, or drops it if they chose nowhere.
fn settle(staged: &Path, to: Option<PathBuf>) {
    if let Some(to) = to {
        // The temp dir is often another file system than the one chosen: then it is a copy.
        let moved = std::fs::rename(staged, &to).or_else(|_| std::fs::copy(staged, &to).map(drop));
        if let Err(e) = moved {
            tracing::warn!(error = %e, to = %to.display(), "could not save a download");
            return; // left staged rather than lost
        }
    }
    discard(staged);
}

/// The save dialog for a staged download, on the window it came from.
fn ask(webview: &Webview, staged: PathBuf) {
    let name = staged.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let start = webview.app_handle().path().download_dir().ok();
    let window = webview.window();

    #[cfg(target_os = "linux")]
    {
        // GTK's own dialog, on the thread GTK runs on.
        let ran = webview.app_handle().run_on_main_thread(move || {
            use gtk::prelude::*;
            let parent = window.gtk_window().ok();
            let dialog = gtk::FileChooserNative::new(
                Some("Save"),
                parent.as_ref(),
                gtk::FileChooserAction::Save,
                Some("_Save"),
                Some("_Cancel"),
            );
            dialog.set_current_name(&name);
            if let Some(start) = start {
                dialog.set_current_folder(start);
            }
            dialog.set_do_overwrite_confirmation(true);
            dialog.set_modal(true);
            // The dialog keeps itself only while its response handler holds it.
            let kept = std::rc::Rc::new(std::cell::RefCell::new(Some(dialog.clone())));
            dialog.connect_response(move |dialog, response| {
                let to = (response == gtk::ResponseType::Accept).then(|| dialog.filename()).flatten();
                settle(&staged, to);
                kept.borrow_mut().take();
            });
            dialog.show();
        });
        if let Err(e) = ran {
            tracing::warn!(error = %e, "could not ask where to save a download");
        }
    }

    #[cfg(not(target_os = "linux"))]
    tauri::async_runtime::spawn(async move {
        let mut dialog = rfd::AsyncFileDialog::new().set_file_name(name).set_parent(&window);
        if let Some(start) = start {
            dialog = dialog.set_directory(start);
        }
        let to = dialog.save_file().await.map(|f| f.path().to_owned());
        settle(&staged, to);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_is_staged_under_its_own_name_and_settled_where_chosen() {
        let temp = tempfile::tempdir().unwrap();
        let (a, b) = (
            stage(temp.path(), Path::new("Deck.pptx")).unwrap(),
            stage(temp.path(), Path::new("Deck.pptx")).unwrap(),
        );
        assert_ne!(a.parent(), b.parent());
        assert_eq!(a.file_name().unwrap(), "Deck.pptx");

        std::fs::write(&a, b"slides").unwrap();
        let to = temp.path().join("Talk.pptx");
        settle(&a, Some(to.clone()));
        assert_eq!(std::fs::read(&to).unwrap(), b"slides");
        assert!(!a.parent().unwrap().exists());

        std::fs::write(&b, b"slides").unwrap();
        settle(&b, None);
        assert!(!b.parent().unwrap().exists());
    }
}
