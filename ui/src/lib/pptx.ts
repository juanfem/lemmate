// A deck saved as PowerPoint, made from what the render pane shows (SPEC §5.6). Quarto's own
// `pptx` goes through pandoc and keeps the words but not the slides' look; this keeps the look:
// the deck is laid out to print in a hidden frame, and dom-to-pptx makes each page a slide of
// native shapes and text boxes, in the fonts the deck loaded. It runs in the reader's browser
// (or the desktop app's webview), so it needs neither Chrome nor anything on the server.

import JSZip from 'jszip'
import { hiddenFrame, inPrintLayout, printLayout } from './printlayout.ts'

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

/**
 * The page for the export frame: the deck in its print view, with Lemmate's print layout (the
 * one the slides PDF uses, without its print dialog), then the export script.
 */
export function exportPage(page: string, layout: string, frameScript: string): string {
  return inPrintLayout(page, layout, { deck: true, dialog: false, then: `<script>${frameScript.replaceAll('</script', '<\\/script')}<\/script>` })
}

/** How long a deck may take to lay out and become a file before the export gives up. */
const TIMEOUT = 120_000

/** The deck `page` (as the render pane shows it) as a PowerPoint file, made in a hidden frame. */
export async function deckToPptx(page: string): Promise<Blob> {
  const [layout, { default: script }, { default: bundle }] = await Promise.all([
    printLayout(),
    import('./pptx-frame.js?raw'),
    import('../vendor/dom-to-pptx.js?raw'),
  ])
  const frame = hiddenFrame()
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
