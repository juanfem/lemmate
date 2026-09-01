import { createHash } from 'node:crypto'
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { defineConfig, type Plugin } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

/**
 * Emit `dist/sw.js` from `sw.js`, with the precache list filled in from what this build
 * actually produced rather than a list kept in step by hand.
 *
 * Source maps are excluded: they are several times the size of the app and no use to an
 * offline reader. The version is a hash of the file list *and* of `index.html`, because the
 * asset names alone would not change if only the document did.
 */
function serviceWorker(): Plugin {
  return {
    name: 'lemmate-service-worker',
    apply: 'build',
    generateBundle(_options, bundle) {
      const emitted = Object.keys(bundle)
        .filter((f) => !f.endsWith('.map'))
        // KaTeX ships every face three times over and its stylesheet lists woff2 first, so the
        // other two are never fetched by anything that can run this app. Precaching them
        // tripled the download for nothing — and since `addAll` is atomic, tripled the number
        // of requests that could fail and leave the install with no offline shell at all. They
        // stay reachable: a miss on any of them is cached by the fetch handler like anything else.
        .filter((f) => !f.endsWith('.woff') && !f.endsWith('.ttf'))
        .map((f) => `/${f}`)
      // Of the static files, only the small ones the running app or its tab actually uses. The
      // launcher icons are half a megabyte and are read by the operating system at install
      // time, while it is online and before there is a worker; storing them for offline use
      // would be paying for something nothing offline ever asks for.
      const statics = readdirSync(join(import.meta.dirname, 'public'))
        .filter((f) => !/^(icon-|apple-touch-icon)/.test(f))
        .map((f) => `/${f}`)
      // '/' is what a navigation requests and '/index.html' is what the server answers with;
      // cache both, because the fallback looks for either and rolldown does not put the
      // document in `bundle` at this point.
      const shell = ['/', '/index.html', ...new Set([...emitted, ...statics])].sort()
      const version = createHash('sha256')
        .update(shell.join('\n'))
        // The asset names cover every change to the app's code, but not one confined to the
        // document — a new meta tag would otherwise ship under the old cache name.
        .update(readFileSync(join(import.meta.dirname, 'index.html'), 'utf8'))
        .digest('hex')
        .slice(0, 12)
      this.emitFile({
        type: 'asset',
        fileName: 'sw.js',
        // `replaceAll`, not `replace`: a single substitution silently takes the first match,
        // which is the wrong one the moment anything above the code mentions the token.
        source: readFileSync(join(import.meta.dirname, 'sw.js'), 'utf8')
          .replaceAll('__VERSION__', version)
          .replaceAll('__SHELL__', JSON.stringify(shell, null, 2)),
      })
    },
  }
}

// Dev: `npm run dev` proxies API and sync WebSocket to a local lemmate-server on :8080.
// Build: `npm run build` → dist/, which lemmate-server serves at / when --web-dir points at it.
export default defineConfig({
  plugins: [svelte(), serviceWorker()],
  build: { outDir: 'dist', emptyOutDir: true, sourcemap: true },
  server: {
    port: 5173,
    proxy: {
      '/api': 'http://127.0.0.1:8080',
      '/ws': { target: 'ws://127.0.0.1:8080', ws: true },
    },
  },
})
