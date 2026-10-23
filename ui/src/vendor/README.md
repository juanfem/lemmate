# Vendored code

## dom-to-pptx.js

[dom-to-pptx](https://github.com/atharva9167j/dom-to-pptx) 2.1.2 (MIT), its browser bundle
`dist/dom-to-pptx.bundle.js`, which carries PptxGenJS, JSZip, opentype.js and a woff2 decoder
inside. It turns a deck's print layout into a PowerPoint file, in the render's own frame
(`src/lib/pptx.ts`).

It is a copy rather than an npm dependency because the package depends on puppeteer, which
would download a Chrome into every `npm ci` for a library the app only runs in a page.

One change, marked `Lemmate:` in the file: in `isTextContainer`, an inline-block whose children
include a block is a box, not a run of text. Quarto's columns and reveal.js's lists are
inline-block; taken for text, their parent became one flat text box and the bullets were lost.

To update: copy the new `dist/dom-to-pptx.bundle.js` over it, put the licence header back, and
apply the change again (`test/pptx.test.ts` fails while it is missing).
