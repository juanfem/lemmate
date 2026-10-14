//! Spell checking in the webview.
//!
//! The page asks for it (`spellcheck="true"` on the editor, see `ui/src/lib/spellcheck.ts`), and
//! WKWebView and WebView2 answer with the system's checker. WebKitGTK does not: its checker is off
//! in every web context until the application turns it on and names the dictionaries — Enchant's,
//! so hunspell or aspell ones the system has installed. The suggestions are then in WebKit's own
//! right-click menu, which the editor shows on Shift+right-click.

/// Overrides the languages taken from the locale: `en_US,es_ES`, or `none` to leave it off.
pub const LANGUAGES_VAR: &str = "LEMMATE_SPELL_LANGUAGES";

/// Turn WebKitGTK's checker on for `window`'s web context. Elsewhere there is nothing to do.
pub fn enable<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    #[cfg(target_os = "linux")]
    {
        let langs = languages(|k| std::env::var(k).ok());
        if langs.is_empty() {
            return;
        }
        let done = window.with_webview(move |wv| {
            use webkit2gtk::{WebContextExt, WebViewExt};
            let Some(ctx) = wv.inner().context() else { return };
            let langs: Vec<&str> = langs.iter().map(String::as_str).collect();
            ctx.set_spell_checking_languages(&langs);
            ctx.set_spell_checking_enabled(true);
        });
        if let Err(e) = done {
            tracing::warn!(error = %e, "could not turn on spell checking");
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = window;
}

/// The dictionaries to check against: [`LANGUAGES_VAR`] if set, else the POSIX locale
/// (`LANGUAGE`'s list, then `LC_ALL`, `LC_MESSAGES`, `LANG`) as `ll_CC` names, else `en_US`.
pub fn languages(env: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let set = |k: &str| env(k).filter(|v| !v.trim().is_empty());
    if let Some(v) = set(LANGUAGES_VAR) {
        if v.trim().eq_ignore_ascii_case("none") {
            return Vec::new();
        }
        return dedup(v.split([',', ':', ' ']).filter_map(locale_name));
    }
    let mut found: Vec<String> = Vec::new();
    // `LANGUAGE` is only a preference list on top of the locale, and is ignored under C/POSIX.
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|k| set(k));
    if let Some(list) = set("LANGUAGE")
        && locale.as_deref().and_then(locale_name).is_some()
    {
        found.extend(list.split(':').filter_map(locale_name));
    }
    found.extend(locale.as_deref().and_then(locale_name));
    if found.is_empty() {
        found.push("en_US".into());
    }
    dedup(found.into_iter())
}

/// `es_ES.UTF-8@euro` → `es_ES`; `C`, `POSIX` and nothing → none.
fn locale_name(raw: &str) -> Option<String> {
    let name = raw.trim().split(['.', '@']).next()?.replace('-', "_");
    let lang = name.split('_').next()?;
    if lang.len() < 2 || !lang.chars().all(|c| c.is_ascii_alphabetic()) || name.eq_ignore_ascii_case("posix")
    {
        return None;
    }
    Some(name)
}

fn dedup(names: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        if !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn the_locale_names_the_dictionary() {
        assert_eq!(languages(env(&[("LANG", "es_ES.UTF-8")])), ["es_ES"]);
        assert_eq!(languages(env(&[("LANG", "de_DE.UTF-8@euro"), ("LC_ALL", "fr_FR.UTF-8")])), ["fr_FR"]);
    }

    #[test]
    fn language_lists_more_and_the_locale_comes_last() {
        let e = env(&[("LANGUAGE", "en_GB:es"), ("LANG", "es_ES.UTF-8")]);
        assert_eq!(languages(e), ["en_GB", "es", "es_ES"]);
    }

    #[test]
    fn a_c_locale_falls_back_to_english() {
        assert_eq!(languages(env(&[("LANG", "C.UTF-8"), ("LANGUAGE", "de")])), ["en_US"]);
        assert_eq!(languages(env(&[("LC_ALL", "POSIX")])), ["en_US"]);
        assert_eq!(languages(env(&[])), ["en_US"]);
    }

    #[test]
    fn the_variable_overrides_the_locale_or_turns_it_off() {
        let e = env(&[(LANGUAGES_VAR, "en_US, es-ES"), ("LANG", "de_DE.UTF-8")]);
        assert_eq!(languages(e), ["en_US", "es_ES"]);
        assert!(languages(env(&[(LANGUAGES_VAR, "none"), ("LANG", "de_DE.UTF-8")])).is_empty());
    }
}
