// Spell checking in the editor. CodeMirror turns the browser's checker off on its content by
// default; we turn it back on unless the reader has said otherwise. Like the measure and the
// palette, a preference of this browser rather than of any note. On is the default and is
// stored as nothing; only off is written.
//
// The checking itself is the platform's — the browser's on the web, the webview's on the
// desktop (WebKitGTK only does it when the shell asks, see `crates/desktop`).

export const SPELLCHECK_KEY = 'lemmate.spellcheck'

/** The stored choice. Storage that holds nothing, holds something else, or cannot be read, says on. */
export function readSpellcheck(storage: Pick<Storage, 'getItem'> | undefined): boolean {
  try {
    return storage?.getItem(SPELLCHECK_KEY) !== 'off'
  } catch {
    return true
  }
}

export function writeSpellcheck(storage: Pick<Storage, 'setItem' | 'removeItem'> | undefined, on: boolean): void {
  try {
    if (on) storage?.removeItem(SPELLCHECK_KEY)
    else storage?.setItem(SPELLCHECK_KEY, 'off')
  } catch {
    /* storage may be unavailable: the choice then lasts as long as the page */
  }
}
