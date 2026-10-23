import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import JSZip from 'jszip'
import { afterHeadStart, exportPage, PRINT_VIEW, shrinkFonts } from '../src/lib/pptx.ts'

const read = (path: string) => readFileSync(new URL(path, import.meta.url), 'utf8')

test('the print view goes first in the head, before reveal.js can initialise', () => {
  assert.equal(afterHeadStart('<html><head lang="en"><script src=r.js></script></head>', '<s/>'), '<html><head lang="en"><s/><script src=r.js></script></head>')
  assert.equal(afterHeadStart('<html><header></header>', '<s/>'), '<s/><html><header></header>')
})

test('the export page is the deck in its print view, its print layout without a dialog, then the export', () => {
  const deck = '<html><head></head><body><div class="reveal"></div><script>x="</body>"</script></body></html>'
  const page = exportPage(deck, '<script>if (__DIALOG__) print()</script>', 'go("</script>")')
  assert.ok(page.startsWith(`<html><head>${PRINT_VIEW}</head>`))
  assert.ok(page.endsWith('<script>if (false) print()</script><script>go("<\\/script>")</script></body></html>'))
  assert.ok(page.includes('x="</body>"'), 'the last </body> is the one that counts')
})

test('the shared print layout is the one the Rust side prints with', () => {
  const layout = read('../../crates/core/src/print-layout.html')
  assert.ok(layout.includes('__DIALOG__'))
  assert.ok(layout.includes("setAttribute('data-lemmate-printed'"))
  assert.match(read('../../crates/core/src/quarto.rs'), /PRINT_SCRIPT: &str = include_str!\("print-layout\.html"\)/u)
  assert.ok(read('../src/lib/pptx-frame.js').includes("hasAttribute('data-lemmate-printed')"))
})

test('the vendored dom-to-pptx keeps an inline-block holding blocks a box', () => {
  const bundle = read('../src/vendor/dom-to-pptx.js')
  assert.ok(bundle.includes("(display.startsWith('inline-') && Array.from(el.children).some((c) => !getComputedStyle(c).display.startsWith('inline')))"))
  assert.ok(!/<\/script/iu.test(bundle))
})

test('every font size in the slides shrinks, and nothing else does', async () => {
  const zip = new JSZip()
  zip.file('ppt/slides/slide1.xml', '<a:rPr lang="en-US" sz="2400" b="1"/><a:endParaRPr sz="1000"/><a:defRPr sz="104"/><a:ext cx="2400"/>')
  zip.file('ppt/slideLayouts/slideLayout1.xml', '<a:rPr sz="2400"/>')
  const out = await JSZip.loadAsync(await shrinkFonts(await zip.generateAsync({ type: 'uint8array' })))
  assert.equal(await out.file('ppt/slides/slide1.xml')!.async('string'), '<a:rPr lang="en-US" sz="2304" b="1"/><a:endParaRPr sz="960"/><a:defRPr sz="100"/><a:ext cx="2400"/>')
  assert.equal(await out.file('ppt/slideLayouts/slideLayout1.xml')!.async('string'), '<a:rPr sz="2400"/>')
})
