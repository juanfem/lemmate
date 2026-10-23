// The PowerPoint export, as it runs inside the render's sandboxed frame (`pptx.ts` puts it there,
// after `print-layout.html`). The deck is in its print layout — every slide laid out at once on a
// page of its own — and this makes that picture into a PowerPoint file with dom-to-pptx: one
// shape per box, a text box per paragraph or list, in the fonts the deck itself loaded. The app
// sends the dom-to-pptx bundle in; the file goes back out the same way, since a sandboxed
// document can neither reach the app nor download anything.
;(function () {
  const ready = () => document.documentElement.hasAttribute('data-lemmate-printed')
  const printed = new Promise((ok) => {
    const t = setInterval(() => {
      if (ready()) (clearInterval(t), ok())
    }, 100)
  })
  const bundle = new Promise((ok) => {
    window.addEventListener('message', (e) => {
      if (e.source === parent && typeof e.data?.lemmatePptxBundle === 'string') ok(e.data.lemmatePptxBundle)
    })
  })

  /**
   * Every @font-face the page declares. A stylesheet Quarto embedded as a data: URL cannot be
   * read through the CSSOM from this frame's opaque origin, so it is fetched and parsed again.
   */
  async function fontFaces() {
    const faces = []
    const take = (rules) => {
      for (const r of rules) {
        if (r instanceof CSSFontFaceRule) {
          const family = r.style.getPropertyValue('font-family').trim().replace(/^["']|["']$/g, '')
          const urls = [...r.style.getPropertyValue('src').matchAll(/url\(\s*(["']?)(.*?)\1\s*\)(?:\s*format\(\s*["']?([\w-]+))?/g)]
            .map((m) => ({ url: m[2], format: (m[3] ?? '').toLowerCase() }))
          const pick =
            urls.find((u) => u.format === 'truetype' || /\.ttf$/i.test(u.url)) ??
            urls.find((u) => u.format === 'woff2' || /woff2/i.test(u.url)) ??
            urls[0]
          if (!pick) continue
          const type =
            pick.format === 'truetype' ? 'ttf'
            : pick.format === 'opentype' ? 'otf'
            : pick.format || (/woff2/i.test(pick.url) ? 'woff2' : /woff/i.test(pick.url) ? 'woff' : 'ttf')
          faces.push({
            family,
            weight: r.style.getPropertyValue('font-weight').trim() || '400',
            style: (r.style.getPropertyValue('font-style').trim() || 'normal').split(' ')[0],
            url: new URL(pick.url, r.parentStyleSheet?.href ?? document.baseURI).href,
            type,
          })
        } else if (r.cssRules) take(r.cssRules)
      }
    }
    for (const sheet of document.styleSheets) {
      try {
        take(sheet.cssRules)
        continue
      } catch {
        /* not readable from here: read it again below */
      }
      const href = sheet.href ?? sheet.ownerNode?.getAttribute?.('href')
      if (!href) continue
      try {
        const copy = new CSSStyleSheet()
        copy.replaceSync((await (await fetch(href)).text()).replace(/@import[^;]+;/g, ''))
        take(copy.cssRules)
      } catch (e) {
        console.warn('PowerPoint export: cannot read a stylesheet', href.slice(0, 80), e)
      }
    }
    return faces
  }

  const WEIGHT_NAMES = { 100: 'Thin', 200: 'ExtraLight', 300: 'Light', 500: 'Medium', 600: 'SemiBold', 800: 'ExtraBold', 900: 'Black' }

  async function run() {
    await printed
    const pages = [...document.querySelectorAll('.reveal .pdf-page')]
    if (!pages.length) throw new Error('the deck did not lay out its slides to print')

    // The print layout scales each page's footer and logo with CSS zoom; dom-to-pptx takes the
    // zoomed box but the unzoomed font size, and the text came out ~1.4× too big. Bake the zoom
    // into the lengths and pin the box where it was drawn.
    const lengths = ['font-size', 'line-height', ...['top', 'right', 'bottom', 'left'].flatMap((s) => [`padding-${s}`, `margin-${s}`])]
    for (const el of document.querySelectorAll('.pdf-page [style*="zoom"]')) {
      const z = parseFloat(el.style.zoom)
      if (!(z > 0) || z === 1) continue
      const at = el.closest('.pdf-page').getBoundingClientRect()
      const r = el.getBoundingClientRect()
      const all = [el, ...el.querySelectorAll('*')]
      const was = all.map((e) => {
        const c = getComputedStyle(e)
        return lengths.map((p) => c.getPropertyValue(p))
      })
      all.forEach((e, i) =>
        lengths.forEach((p, j) => {
          if (was[i][j].endsWith('px')) e.style.setProperty(p, `${parseFloat(was[i][j]) * z}px`, 'important')
        }),
      )
      el.style.zoom = 1
      Object.assign(el.style, {
        left: `${r.left - at.left}px`,
        top: `${r.top - at.top}px`,
        width: `${r.width}px`,
        height: `${r.height}px`,
        right: 'auto',
        bottom: 'auto',
      })
      el.style.setProperty('margin', '0', 'important')
    }

    // Layout dom-to-pptx flattens, rewritten into the same picture with plain boxes. A CSS
    // multi-column list would become one single-column text box: one list per column instead,
    // side by side with the same gap.
    for (const list of document.querySelectorAll('.pdf-page ul, .pdf-page ol')) {
      const cs = getComputedStyle(list)
      if (!(+cs.columnCount > 1)) continue
      const cols = new Map()
      for (const li of list.children) {
        const left = Math.round(li.getBoundingClientRect().left)
        if (!cols.has(left)) cols.set(left, [])
        cols.get(left).push(li)
      }
      if (cols.size < 2) continue
      const row = document.createElement('div')
      row.style.cssText = `display: flex; gap: ${cs.columnGap}; margin: ${cs.margin};`
      for (const [, items] of [...cols].sort((a, b) => a[0] - b[0])) {
        const col = list.cloneNode(false)
        col.style.cssText += '; columns: auto; flex: 1 1 0; margin: 0;'
        items.forEach((li) => col.appendChild(li))
        row.appendChild(col)
      }
      list.replaceWith(row)
    }
    // Flex rows: PowerPoint knows neither baseline alignment nor where justify-content put a box.
    const flat = (el) => ![...el.querySelectorAll('*')].some((d) => !getComputedStyle(d).display.startsWith('inline'))
    for (const row of document.querySelectorAll('.pdf-page *')) {
      const cs = getComputedStyle(row)
      if (!cs.display.endsWith('flex') || cs.flexDirection.startsWith('column')) continue
      // Text directly in a row (split by <br> at most) comes out as one unaligned box per line:
      // one item instead, aligned the way justify-content placed it.
      if (
        [...row.children].every((k) => k.tagName === 'BR') &&
        [...row.childNodes].some((n) => n.nodeType === Node.TEXT_NODE && n.textContent.trim())
      ) {
        const item = document.createElement('span')
        item.style.textAlign = { 'flex-end': 'right', end: 'right', center: 'center' }[cs.justifyContent] ?? cs.textAlign
        item.append(...row.childNodes)
        row.append(item)
      }
      const kids = [...row.children].filter((k) => k.textContent.trim() && flat(k))
      if (!kids.length) continue
      // Items on one baseline get the row's line height, so PowerPoint sets their text on the
      // same baseline; what that adds to the row comes off its bottom margin, so nothing moves.
      if (cs.alignItems === 'baseline') {
        const lh = Math.max(...kids.map((k) => k.getBoundingClientRect().height))
        const was = row.getBoundingClientRect().height
        kids.forEach((k) => (k.style.lineHeight = `${lh}px`))
        const grew = row.getBoundingClientRect().height - was
        if (grew) row.style.marginBottom = `${parseFloat(cs.marginBottom) - grew}px`
      }
      // An item pushed to the right edge is right-aligned text.
      const jc = cs.justifyContent
      const right =
        jc === 'flex-end' || jc === 'end' ? kids
        : jc === 'space-between' && row.children.length > 1 && kids.includes(row.lastElementChild) ? [row.lastElementChild]
        : []
      right.forEach((k) => (k.style.textAlign = 'right'))
    }
    // A float pins its box to an edge the same way.
    for (const el of document.querySelectorAll('.pdf-page *')) {
      const float = getComputedStyle(el).float
      if ((float === 'right' || float === 'left') && el.textContent.trim() && flat(el)) el.style.textAlign = float
    }

    // PowerPoint embeds four faces per family: regular, bold, italic, bold italic. A face of any
    // other weight becomes a family of its own at 400, named the way fonts name their legacy
    // families ("Inter Medium"). Each element gets the face the browser would match, from a
    // snapshot taken first so that a child does not inherit its parent's rewritten family. A
    // variable font's weight range is left alone: PowerPoint cannot embed one.
    const byFamily = new Map()
    for (const f of await fontFaces()) {
      if (/\s/.test(f.weight) || !(parseFloat(f.weight) > 0)) continue
      f.w = parseFloat(f.weight)
      if (!byFamily.has(f.family)) byFamily.set(f.family, [])
      byFamily.get(f.family).push(f)
    }
    // CSS font matching, roughly: at or below 500 the lighter faces come first, above it the heavier.
    const distance = (w, f) => {
      const x = f.w - w
      return w <= 500 ? (x <= 0 ? -x : 1000 + x) : x >= 0 ? x : 1000 - x
    }
    const els = [...document.querySelectorAll('.pdf-page, .pdf-page *')]
    const want = els.map((e) => {
      const c = getComputedStyle(e)
      return {
        family: c.fontFamily.split(',')[0].trim().replace(/^["']|["']$/g, ''),
        weight: +c.fontWeight,
        style: c.fontStyle === 'normal' ? 'normal' : 'italic',
      }
    })
    const used = new Map()
    els.forEach((e, i) => {
      const { family, weight, style } = want[i]
      const all = byFamily.get(family)
      if (!all) return
      const same = all.filter((f) => f.style === style)
      const face = (same.length ? same : all).reduce((a, b) => (distance(weight, b) < distance(weight, a) ? b : a))
      const slot = face.w === 400 || face.w === 700
      const name = slot ? family : `${family} ${WEIGHT_NAMES[face.w] ?? face.w}`
      const w = slot ? face.w : 400
      e.style.setProperty('font-family', `"${name}"`, 'important')
      e.style.setProperty('font-weight', String(w), 'important')
      used.set(`${name}|${w}|${face.style}`, { name, weight: w, style: face.style, face })
    })
    const sheet = new CSSStyleSheet()
    for (const u of used.values()) {
      if (u.name !== u.face.family) {
        sheet.insertRule(`@font-face { font-family: "${u.name}"; src: url("${u.face.url}"); font-weight: ${u.weight}; font-style: ${u.style}; }`)
      }
    }
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet]
    await Promise.all([...used.values()].map((u) => document.fonts.load(`${u.style} ${u.weight} 16px "${u.name}"`).catch(() => {})))
    await document.fonts.ready

    // The print layout centred the slides on the faces it had; centre again the same way.
    const cfg = Reveal.getConfig()
    for (const page of pages) {
      const s = page.querySelector('section')
      const room = page.clientHeight
      if (s && (cfg.center || s.classList.contains('center')) && s.offsetHeight <= room) {
        s.style.top = `${Math.max((room - s.offsetHeight) / 2, 0)}px`
      }
    }

    // Speaker notes go to the slide's notes, not onto it; the slide number PowerPoint has its own.
    for (const p of pages) {
      const notes = p.querySelector('aside.notes')
      if (notes) {
        p.setAttribute('data-pptx-notes', notes.innerText.trim())
        notes.remove()
      }
      p.querySelectorAll('.speaker-notes-pdf, .slide-number-pdf').forEach((e) => e.remove())
    }

    // Added as a script element's text, not parsed as HTML: nothing in it can end the element.
    const code = document.createElement('script')
    code.textContent = await bundle
    document.head.appendChild(code)
    // dom-to-pptx takes a font's type from its URL's extension, and a data: URL has none: a
    // fragment gives it one.
    const fonts = [...used.values()].map((u) => ({ name: u.name, weight: u.weight, style: u.style, url: `${u.face.url}#font.${u.face.type}` }))
    const w = parseFloat(cfg.width)
    const h = parseFloat(cfg.height)
    const blob = await window.domToPptx.exportToPptx(pages, {
      skipDownload: true,
      width: 13.333,
      height: Math.round((1333.3 * h) / w) / 100,
      autoEmbedFonts: false,
      fonts,
    })
    const bytes = await blob.arrayBuffer()
    parent.postMessage({ lemmatePptx: bytes }, '*', [bytes])
  }

  run().catch((e) => parent.postMessage({ lemmatePptxError: String(e?.message ?? e) }, '*'))
})()
