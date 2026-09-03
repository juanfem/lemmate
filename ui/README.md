# lemmate-ui

TypeScript side of the project. Today: the markdown indexer that must agree with `lemmate-core`
on the shared `corpus/`. Later: the CodeMirror 6 live-preview editor and the Svelte 5 shell
used by the desktop and web clients.

```sh
npm install
npm run dev       # Vite dev server on :5173, proxying /api and /ws to a lemmate-server on :8080
npm run build     # → dist/, serve with `lemmate-server --web-dir ui/dist`
npm test          # corpus conformance + (with LEMMATE_SERVER_BIN/LEMMATE_CLI_BIN set) a live sync e2e
npm run check     # svelte-check + tsc
```

Layout: `src/lib/sync.ts` (frame-protocol Yjs provider), `src/lib/vault.svelte.ts` (reactive vault
session), `src/lib/editor/` (CodeMirror 6: syntax extensions, live preview, setup),
`src/components/` (tree, tabs, quick switcher, search), `src/App.svelte` (shell).

Requires Node ≥ 24.
