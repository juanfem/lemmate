// A render laid out to be printed, in a hidden frame of the app's own: what the slides PDF's
// fallback, the printer on a page, and the PowerPoint export (`pptx.ts`) all start from. It is
// the server's `quarto::for_print`, done here on the page the pane already has.

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
 * `page` laid out to print with Lemmate's print layout (`print-layout.html`, shared with the
 * server's slides PDF): a deck in reveal.js's print view, a slide to a page. With `dialog` the
 * page opens the print dialog itself once it is laid out; `then` is markup to run after it.
 */
export function inPrintLayout(page: string, printLayout: string, opts: { deck: boolean; dialog: boolean; then?: string }): string {
  const layout = printLayout.replaceAll('__DIALOG__', opts.dialog ? 'true' : 'false')
  return beforeBodyEnd(opts.deck ? afterHeadStart(page, PRINT_VIEW) : page, layout + (opts.then ?? ''))
}

/** Lemmate's print layout, loaded when first wanted. */
export async function printLayout(): Promise<string> {
  return (await import('../../../crates/core/src/print-layout.html?raw')).default
}

/**
 * A hidden frame for `srcdoc`, sandboxed like the pane's own (`allow-scripts`, plus whatever
 * `allow` adds) and laid over the page invisibly rather than off it: a browser stops drawing a
 * cross-origin frame it cannot see, and reveal.js waits on that to lay out.
 */
export function hiddenFrame(allow = ''): HTMLIFrameElement {
  const frame = document.createElement('iframe')
  frame.setAttribute('sandbox', `allow-scripts${allow ? ` ${allow}` : ''}`)
  frame.setAttribute('aria-hidden', 'true')
  frame.tabIndex = -1
  frame.style.cssText = 'position:fixed;left:0;top:0;width:1280px;height:720px;border:0;opacity:0;pointer-events:none;z-index:-1'
  return frame
}

/** What the print frame says as its print dialog opens, and once it has closed, printed or not. */
const SAY_PRINTING = `<script>addEventListener("beforeprint",function(){parent.postMessage({lemmatePrint:"open"},"*")});addEventListener("afterprint",function(){parent.postMessage({lemmatePrint:"closed"},"*")})<\/script>`
/** A dialog left open longer than this is the reader's business; the frame goes anyway. */
const PRINT_WAIT = 30 * 60_000

/**
 * Print `page` from inside the app: it is laid out to print in a hidden frame, which opens the
 * browser's (or the webview's) print dialog itself — *Save as PDF* there keeps what was on screen.
 * Only that frame may open dialogs (`allow-modals`), not the pane's, and it goes once its dialog
 * has. Resolves as the dialog opens: until then the page is still being laid out.
 */
export async function printHere(page: string, deck: boolean): Promise<void> {
  const layout = await printLayout()
  const frame = hiddenFrame('allow-modals')
  return new Promise<void>((opened) => {
    const finish = () => {
      clearTimeout(timer)
      window.removeEventListener('message', onMessage)
      frame.remove()
      opened()
    }
    const onMessage = (e: MessageEvent) => {
      if (e.source !== frame.contentWindow) return
      const said = (e.data as { lemmatePrint?: unknown } | null)?.lemmatePrint
      if (said === 'open') opened()
      else if (said === 'closed') finish()
    }
    const timer = setTimeout(finish, PRINT_WAIT)
    window.addEventListener('message', onMessage)
    frame.srcdoc = inPrintLayout(page, layout, { deck, dialog: true, then: SAY_PRINTING })
    document.body.appendChild(frame)
  })
}
