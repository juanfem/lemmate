//! Printing a render on macOS, where the page cannot do it itself.
//!
//! Everywhere else the app prints a render from a hidden frame of its own, which opens the
//! webview's print dialog (`ui/src/lib/printlayout.ts`). WKWebView opens nothing for a page's
//! `window.print()` — WebKit hands that to a private delegate method wry does not implement — so
//! the page asks the shell instead ([`super::ShellRequest::Print`]): a window opens on the
//! render laid out to print (`?print-pdf`, `quarto::for_print`), and once the page says it is
//! laid out, the native print panel opens on it. A deck goes on paper the shape of its slides —
//! the print panel would otherwise pick the default paper, A4 or Letter, which is why the slides
//! PDF is not left to a dialog elsewhere either.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

/// Given to the print window's page before its own scripts: the page's print dialog is the
/// native panel, not its own `window.print()`; and once laid out (`data-lemmate-printed`) it says
/// so in its title, with a deck's page size in CSS pixels — what [`page_size`] reads.
pub const PRINT_HOOK: &str = r#"window.print = function () {}
;(function wait() {
  var d = document.documentElement
  if (!d || !d.hasAttribute('data-lemmate-printed')) return void setTimeout(wait, 100)
  var p = document.querySelector('.reveal .pdf-page'), r = p && p.getBoundingClientRect()
  document.title = 'lemmate-print:' + (r ? Math.round(r.width) + 'x' + Math.round(r.height) : 'page')
})()"#;

const TITLE_PREFIX: &str = "lemmate-print:";

/// What the page's title says once it is laid out: `Some(Some((w, h)))` for a deck's page size in
/// CSS pixels, `Some(None)` for a page (default paper), `None` while it is not laid out yet.
pub fn page_size(title: &str) -> Option<Option<(f64, f64)>> {
    let said = title.strip_prefix(TITLE_PREFIX)?;
    if said == "page" {
        return Some(None);
    }
    let (w, h) = said.split_once('x')?;
    let (w, h) = (w.parse::<f64>().ok()?, h.parse::<f64>().ok()?);
    (w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0).then_some(Some((w, h)))
}

#[cfg(target_os = "macos")]
pub use mac::open;

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use tauri::{Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

    use super::{PRINT_HOOK, page_size};

    /// How long the panel gets to appear before the window is left to the reader to close.
    const PANEL_WAIT: Duration = Duration::from_secs(30);

    /// A window on `url` (the relay's render page, `?print-pdf`), printed natively once laid out
    /// and closed again once the print panel has gone. It shows the layout meanwhile: macOS
    /// attaches the panel to a window, and stops drawing — so laying out — a page it cannot see.
    pub fn open(app: &tauri::AppHandle, url: Url) {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let label = format!("print-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        tracing::info!(%url, label, "printing a render");
        let printed = std::sync::Arc::new(AtomicBool::new(false));
        let (navigator, start) = (app.clone(), url.clone());
        let built = WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
            .title("Print")
            .inner_size(1100.0, 720.0)
            .initialization_script(PRINT_HOOK)
            // The render and nothing else: a link in it goes nowhere from this window.
            .on_navigation(move |to| {
                to.origin() == start.origin()
                    && navigator.try_state::<crate::Relay>().is_some_and(|r| r.serves(to))
            })
            .on_document_title_changed(move |window, title| {
                let Some(page) = page_size(&title) else { return };
                if printed.swap(true, Ordering::SeqCst) {
                    return;
                }
                let _ = window.set_title("Print");
                print(&window, page);
            })
            .build();
        if let Err(e) = built {
            tracing::warn!(error = %e, "could not open a print window");
        }
    }

    fn print(window: &WebviewWindow, page: Option<(f64, f64)>) {
        let ran = window.with_webview(move |webview| {
            // Safety: Tauri hands over the window's own WKWebView and NSWindow, on the main thread.
            unsafe { native::print(webview.inner(), webview.ns_window(), page) }
        });
        if let Err(e) = ran {
            tracing::warn!(error = %e, "could not print");
            return;
        }
        let window = window.clone();
        std::thread::spawn(move || close_after_panel(window));
    }

    /// Closes the print window once its print panel — a sheet on it — has come and gone. One
    /// that never appears leaves the window open: the reader closes it.
    fn close_after_panel(window: WebviewWindow) {
        let since = Instant::now();
        let mut seen = false;
        loop {
            std::thread::sleep(Duration::from_millis(400));
            let (tx, rx) = std::sync::mpsc::channel();
            let asked = window.with_webview(move |webview| {
                // Safety: as in `print`.
                let _ = tx.send(unsafe { native::sheet_open(webview.ns_window()) });
            });
            let Some(open) = asked.ok().and_then(|()| rx.recv_timeout(Duration::from_secs(5)).ok()) else {
                return; // the window has gone
            };
            seen |= open;
            if seen && !open {
                let _ = window.close();
                return;
            }
            if !seen && since.elapsed() > PANEL_WAIT {
                return;
            }
        }
    }

    mod native {
        use std::ffi::c_void;

        use objc2::rc::Retained;
        use objc2_app_kit::{NSPrintInfo, NSPrintingPaginationMode, NSWindow};
        use objc2_foundation::{NSCopying, NSSize};
        use objc2_web_kit::WKWebView;

        /// CSS pixels are 1/96 in, points 1/72.
        const POINTS_PER_PX: f64 = 0.75;

        /// The print panel, as a sheet on `window`, for what `webview` shows: on paper of `page`
        /// CSS pixels with no margins (a deck's slide), or else on the default paper. Pages wider
        /// than the paper are scaled to fit it rather than cut.
        ///
        /// # Safety
        /// `webview` and `window` are Tauri's `PlatformWebview::inner` and `ns_window`, on the
        /// main thread.
        pub unsafe fn print(webview: *mut c_void, window: *mut c_void, page: Option<(f64, f64)>) {
            let webview: &WKWebView = unsafe { &*webview.cast() };
            let window: &NSWindow = unsafe { &*window.cast() };
            // A copy: the shared one is every later print's starting point too.
            let info: Retained<NSPrintInfo> = NSPrintInfo::sharedPrintInfo().copy();
            if let Some((w, h)) = page {
                info.setPaperSize(NSSize::new(w * POINTS_PER_PX, h * POINTS_PER_PX));
                info.setTopMargin(0.0);
                info.setRightMargin(0.0);
                info.setBottomMargin(0.0);
                info.setLeftMargin(0.0);
                info.setHorizontallyCentered(false);
                info.setVerticallyCentered(false);
            }
            info.setHorizontalPagination(NSPrintingPaginationMode::Fit);
            let op = unsafe { webview.printOperationWithPrintInfo(&info) };
            op.setShowsPrintPanel(true);
            op.setShowsProgressPanel(true);
            // Without a frame, WKWebView's print view prints blank pages.
            if let Some(view) = op.view() {
                view.setFrame(webview.frame());
            }
            unsafe {
                op.runOperationModalForWindow_delegate_didRunSelector_contextInfo(
                    window,
                    None,
                    None,
                    std::ptr::null_mut(),
                )
            };
        }

        /// Whether a sheet — the print panel — is attached to `window`.
        ///
        /// # Safety
        /// As for [`print`].
        pub unsafe fn sheet_open(window: *mut c_void) -> bool {
            let window: &NSWindow = unsafe { &*window.cast() };
            window.attachedSheet().is_some()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_says_when_the_page_is_laid_out_and_on_what_paper() {
        assert_eq!(page_size("Evolution of the LHC"), None);
        assert_eq!(page_size("lemmate-print:page"), Some(None));
        assert_eq!(page_size("lemmate-print:1356x762"), Some(Some((1356.0, 762.0))));
        assert_eq!(page_size("lemmate-print:0x762"), None);
        assert_eq!(page_size("lemmate-print:NaNx762"), None);
        assert_eq!(page_size("lemmate-print:1356"), None);
        assert!(PRINT_HOOK.contains(TITLE_PREFIX) && PRINT_HOOK.contains("data-lemmate-printed"));
    }
}
