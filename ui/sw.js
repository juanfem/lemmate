// The offline shell. The two constants below are filled in at build time by the
// serviceWorker() plugin in vite.config.ts, so the precache list is exactly what the build
// emitted and the cache name changes whenever any of it does. (Their placeholder tokens are
// deliberately not repeated in this comment: substitution is textual, and a mention up here
// would be replaced instead of the code — a multi-line array pasted after a `//` is a syntax
// error, and a silently broken worker at that.)
//
// What this does *not* touch is as important as what it caches: `/api/` and `/ws` always go to
// the network and are never stored. Notes come from the CRDT docs `y-indexeddb` already keeps
// (SPEC §6.4), so an offline app reads them out of IndexedDB rather than out of a stale HTTP
// response — caching the REST layer would only invent a second, wronger copy.

const VERSION = '__VERSION__'
const SHELL = __SHELL__
const CACHE = `lemmate-shell-${VERSION}`

self.addEventListener('install', (event) => {
  // A partial precache is worse than none: `addAll` rejects atomically, leaving the old cache
  // in place and this worker un-activated.
  event.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()))
})

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((names) => Promise.all(names.filter((n) => n !== CACHE).map((n) => caches.delete(n))))
      .then(() => self.clients.claim()),
  )
})

/** Store a response only if it is one we could serve again. */
async function put(request, response) {
  if (response.ok && response.type === 'basic') {
    const cache = await caches.open(CACHE)
    await cache.put(request, response.clone())
  }
  return response
}

self.addEventListener('fetch', (event) => {
  const { request } = event
  if (request.method !== 'GET') return
  const url = new URL(request.url)
  if (url.origin !== self.location.origin) return
  // The relay and the server both live behind these; they must fail honestly when offline so
  // the UI can show its "offline" state rather than replay yesterday's answer.
  if (url.pathname.startsWith('/api/') || url.pathname === '/ws') return

  // The document: network first, so a deploy is picked up on the next launch, with the cached
  // shell behind it. Every route is the same SPA entry — the hash carries the note id.
  if (request.mode === 'navigate') {
    event.respondWith(
      fetch(request)
        .then((r) => put(request, r))
        .catch(async () => (await caches.match('/index.html')) ?? (await caches.match('/'))),
    )
    return
  }

  // Everything else is content-addressed by Vite (`/assets/index-<hash>.js`), so a hit is
  // always the right bytes and a miss is always worth storing.
  event.respondWith(
    caches.match(request).then((hit) => hit ?? fetch(request).then((r) => put(request, r))),
  )
})
