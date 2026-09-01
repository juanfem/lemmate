import { mount } from 'svelte'
import 'katex/dist/katex.min.css'
import './app.css'
import App from './App.svelte'

mount(App, { target: document.getElementById('app')! })

// The offline shell (sw.js, emitted by the serviceWorker plugin in vite.config.ts). Build only:
// a dev server has no sw.js, and a worker left over from a previous build would serve yesterday's
// bundle straight over vite's hot reloading. Registration needs a secure context, which https and
// the relay's 127.0.0.1 both are; plain http to a LAN address is not, and fails here quietly.
if (import.meta.env.PROD && 'serviceWorker' in navigator) {
  addEventListener('load', () => {
    navigator.serviceWorker.register('/sw.js').catch(() => {
      /* unsupported, blocked, or an insecure origin: the app just stays online-only */
    })
  })
}
