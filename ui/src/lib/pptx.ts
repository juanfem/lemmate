// A deck saved as PowerPoint, made from what the render pane shows (SPEC §5.6). Quarto's own
// `pptx` goes through pandoc and keeps the words but not the slides' look; this keeps the look:
// the deck is laid out to print in a hidden frame, and dom-to-pptx makes each page a slide of
// native shapes and text boxes, in the fonts the deck loaded. It runs in the reader's browser
// (or the desktop app's webview), so it needs neither Chrome nor anything on the server.

import JSZip from 'jszip'
import { beforeBodyEnd } from './render.ts'

/**
 * What `?print-pdf` does to a reveal.js deck, for a deck in a `srcdoc` frame, which has no URL
 * to carry it: the `view: 'print'` it sets is passed to `Reveal.initialize` instead. It must
 * come before reveal.js, so it waits for the bundle to assign `window.Reveal`.
 */
export const PRINT_VIEW = `<script>(function(){var R;Object.defineProperty(window,"Reveal",{configurable:true,enumerable:true,get:function(){return R},set:function(v){if(v&&typeof v.initialize==="function"&&!v.__lemmatePrint){var init=v.initialize;v.initialize=function(o){return init.call(this,Object.assign({},o,{view:"print"}))};v.__lemmatePrint=true}R=v}})})()<\/script>`

/** `page` with `script` added straight after its opening `<head>` (or at the very start). */
export function afterHeadStart(page: string, script: string): string {
  const m = /<head(?:\s[^>]*)?>/iu.exec(page)
  const at = m ? m.index + m[0].length : 0
  return page.slice(0, at) + script + page.slice(at)
}

/**
 * The page for the export frame: the deck in its print view, with Lemmate's print layout (the
 * one the slides PDF uses, `print-layout.html`, without its print dialog) and the export script.
 */
export function exportPage(page: string, printLayout: string, frameScript: string): string {
  const layout = printLayout.replaceAll('__DIALOG__', 'false')
  return beforeBodyEnd(afterHeadStart(page, PRINT_VIEW), `${layout}<script>${frameScript.replaceAll('</script', '<\\/script')}<\/script>`)
}

/**
 * PowerPoint breaks lines a little differently from the browser, so text that just fits its box
 * on screen can wrap in the deck. Rather than move or resize anything, every font size shrinks
 * by `by`; boxes, positions and line spacing stay where the browser put them. `sz` is in
 * hundredths of a point, on runs, paragraph ends and defaults.
 */
export const SHRINK = 0.04
export async function shrinkFonts(pptx: ArrayBuffer | Uint8Array, by = SHRINK): Promise<Uint8Array> {
  const zip = await JSZip.loadAsync(pptx)
  for (const name of Object.keys(zip.files).filter((n) => /^ppt\/slides\/slide\d+\.xml$/u.test(n))) {
    const xml = await zip.file(name)!.async('string')
    zip.file(
      name,
      xml.replace(/(<a:(?:rPr|endParaRPr|defRPr)\b[^>]*\ssz=")(\d+)"/gu, (_, head: string, sz: string) => `${head}${Math.max(100, Math.round(Number(sz) * (1 - by)))}"`),
    )
  }
  return zip.generateAsync({ type: 'uint8array', compression: 'DEFLATE' })
}

/** How long a deck may take to lay out and become a file before the export gives up. */
const TIMEOUT = 120_000

/**
 * The deck `page` (as the render pane shows it) as a PowerPoint file. The frame is sandboxed
 * like the pane's own, and laid over the page invisibly rather than off it: a browser stops
 * drawing a cross-origin frame it cannot see, and reveal.js waits on that to lay out.
 */
export async function deckToPptx(page: string): Promise<Blob> {
  const [{ default: layout }, { default: script }, { default: bundle }] = await Promise.all([
    import('../../../crates/core/src/print-layout.html?raw'),
    import('./pptx-frame.js?raw'),
    import('../vendor/dom-to-pptx.js?raw'),
  ])
  const frame = document.createElement('iframe')
  frame.setAttribute('sandbox', 'allow-scripts')
  frame.setAttribute('aria-hidden', 'true')
  frame.tabIndex = -1
  frame.style.cssText = 'position:fixed;left:0;top:0;width:1280px;height:720px;border:0;opacity:0;pointer-events:none;z-index:-1'
  let done: (v: ArrayBuffer | Error) => void = () => {}
  const result = new Promise<ArrayBuffer | Error>((ok) => (done = ok))
  const onMessage = (e: MessageEvent) => {
    if (e.source !== frame.contentWindow) return
    const d = e.data as { lemmatePptx?: unknown; lemmatePptxError?: unknown } | null
    if (d?.lemmatePptx instanceof ArrayBuffer) done(d.lemmatePptx)
    else if (typeof d?.lemmatePptxError === 'string') done(new Error(d.lemmatePptxError))
  }
  window.addEventListener('message', onMessage)
  frame.addEventListener('load', () => frame.contentWindow?.postMessage({ lemmatePptxBundle: bundle }, '*'))
  const timer = setTimeout(() => done(new Error('the deck took too long to export')), TIMEOUT)
  frame.srcdoc = exportPage(page, layout, script)
  document.body.appendChild(frame)
  try {
    const out = await result
    if (out instanceof Error) throw out
    const bytes = await shrinkFonts(out)
    return new Blob([bytes as Uint8Array<ArrayBuffer>], { type: 'application/vnd.openxmlformats-officedocument.presentationml.presentation' })
  } finally {
    clearTimeout(timer)
    window.removeEventListener('message', onMessage)
    frame.remove()
  }
}
