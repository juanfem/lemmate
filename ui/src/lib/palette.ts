// The colour palette: Finder's (white page, cool greys, system blue) or warm paper. Like the
// measure, a preference of the reader's screen rather than of any note, so it lives in this
// browser's storage. Finder is the default and is stored as nothing; only warm is written.

export const PALETTE_KEY = 'lemmate.palette'

export type Palette = 'finder' | 'warm'

/** `--bg` of each palette in each scheme, for the browser chrome (`theme-color`). Keep in step with app.css. */
export const PAGE_BG: Record<Palette, { light: string; dark: string }> = {
  finder: { light: '#ffffff', dark: '#1e1e1e' },
  warm: { light: '#fdfcfb', dark: '#1a1816' },
}

/** The stored palette. Storage that holds nothing, holds something else, or cannot be read, says Finder. */
export function readPalette(storage: Pick<Storage, 'getItem'> | undefined): Palette {
  try {
    return storage?.getItem(PALETTE_KEY) === 'warm' ? 'warm' : 'finder'
  } catch {
    return 'finder'
  }
}

export function writePalette(storage: Pick<Storage, 'setItem' | 'removeItem'> | undefined, p: Palette): void {
  try {
    if (p === 'warm') storage?.setItem(PALETTE_KEY, 'warm')
    else storage?.removeItem(PALETTE_KEY)
  } catch {
    /* storage may be unavailable: the choice then lasts as long as the page */
  }
}

/** Put the choice where the stylesheets look for it — `data-palette` on the root element — and
 *  repaint the browser's own chrome to match. */
export function applyPalette(root: HTMLElement | undefined, p: Palette): void {
  if (!root) return
  if (p === 'warm') root.dataset.palette = 'warm'
  else delete root.dataset.palette
  for (const meta of root.ownerDocument.querySelectorAll<HTMLMetaElement>('meta[name="theme-color"]')) {
    meta.content = PAGE_BG[p][meta.media.includes('dark') ? 'dark' : 'light']
  }
}
