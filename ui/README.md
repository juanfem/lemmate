# notes-ui

TypeScript side of the project. Today: the markdown indexer that must agree with `notes-core`
on the shared `corpus/`. Later: the CodeMirror 6 live-preview editor and the Svelte 5 shell
used by the desktop, mobile, and web clients.

```sh
npm install
npm test          # corpus conformance (node:test, TypeScript via Node's type stripping)
npm run typecheck
```

Requires Node ≥ 24.
