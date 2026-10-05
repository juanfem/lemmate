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

/**
 * Where the reader is in a render: the slide of a deck, or how far down a page. The page reports
 * it as it changes, and a re-render starts there rather than at the top — the page is a
 * sandboxed document of its own origin, so asking it is all the app can do.
 */
export type Place = { slide: { indexh: number; indexv: number; indexf?: number } } | { y: number }

/** A place out of a message from the frame — whatever else it carries is dropped. */
export function placeOf(data: unknown): Place | null {
  const p = (data as { lemmateRenderPlace?: unknown } | null)?.lemmateRenderPlace as Record<string, unknown> | undefined
  if (!p || typeof p !== 'object') return null
  const n = (v: unknown) => typeof v === 'number' && Number.isFinite(v) && v >= 0
  const s = p.slide as Record<string, unknown> | undefined
  if (s && typeof s === 'object' && n(s.indexh) && n(s.indexv)) {
    const slide = { indexh: s.indexh as number, indexv: s.indexv as number }
    return { slide: n(s.indexf) ? { ...slide, indexf: s.indexf as number } : slide }
  }
  return n(p.y) ? { y: p.y as number } : null
}

/**
 * The script that keeps the place: it goes back to `at` once the page is ready — a deck to that
 * slide, a page to that height after its images have loaded — and from then on tells the app
 * where the reader is. A place of the other kind (the picker switched page to slides) is left be.
 */
export function keepPlace(at: Place | null): string {
  // Only numbers, from `placeOf`: nothing in it can close the script.
  const start = JSON.stringify(at ? placeOf({ lemmateRenderPlace: at }) : null)
  return `<script>(function(){var at=${start};function tell(p){try{parent.postMessage({lemmateRenderPlace:p},"*")}catch(e){}}
if(document.querySelector(".reveal")){var tries=0;(function hook(){var R=window.Reveal;if(!R||!R.isReady||!R.isReady()){if(++tries<100)setTimeout(hook,100);return}
if(at&&at.slide)R.slide(at.slide.indexh,at.slide.indexv,at.slide.indexf);function send(){var s=R.getIndices();tell({slide:{indexh:s.h||0,indexv:s.v||0,indexf:s.f>=0?s.f:undefined}})}
["slidechanged","fragmentshown","fragmenthidden"].forEach(function(v){R.on(v,send)});send()})();return}
function back(){if(at&&typeof at.y==="number")window.scrollTo(0,at.y);var t=0;window.addEventListener("scroll",function(){clearTimeout(t);t=setTimeout(function(){tell({y:Math.round(window.scrollY)})},150)},{passive:true})}
if(document.readyState==="complete")back();else window.addEventListener("load",back)})()<\/script>`
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
