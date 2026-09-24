// What the render pane does to Quarto's page before showing it (SPEC §5.6).

/**
 * `page` with `script` added just before its closing `</body>` — the *last* one. A reveal.js
 * deck carries another inside its speaker-notes plugin, a string of JavaScript that writes the
 * speaker window; putting the script there cut that string open and spilled the plugin onto
 * the slides as text.
 */
export function beforeBodyEnd(page: string, script: string): string {
  const at = page.lastIndexOf('</body>')
  return at === -1 ? page + script : page.slice(0, at) + script + page.slice(at)
}

/** The page a render tab last showed, and what it was made from. */
export interface KeptRender {
  html: string
  /** What the bar names it: `page` or `slides`. */
  made: string
  renderId: string
  choice: string
  renderedFrom: string | null
}

/**
 * Renders kept past the component that showed them, by vault and note. Moving a tab to another
 * pane makes a new component for it, and running Quarto again for a page the tab
 * already had was seconds of waiting for nothing. A handful is plenty — a self-contained deck
 * can be megabytes — and the least recently shown goes first.
 */
const kept = new Map<string, KeptRender>()
const KEEP = 8

export function keptRender(vault: string, note: string): KeptRender | undefined {
  const key = `${vault}/${note}`
  const r = kept.get(key)
  if (r) {
    kept.delete(key)
    kept.set(key, r)
  }
  return r
}

export function keepRender(vault: string, note: string, r: KeptRender) {
  const key = `${vault}/${note}`
  kept.delete(key)
  kept.set(key, r)
  for (const old of kept.keys()) {
    if (kept.size <= KEEP) break
    kept.delete(old)
  }
}
