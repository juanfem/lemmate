<script lang="ts">
  // A native app asking to be signed in (crates/server/src/apps.rs): the desktop app or
  // `lemmate login --browser` opened this page. Allowing it files a single-use code and sends the
  // browser back to the app's loopback address with it; the app trades the code for an access
  // token named after the device. Cancelling sends it back with `error=access_denied`, so the app
  // stops waiting.
  import { ApiError, type User } from '../lib/api.ts'
  import type { AppAuthorize } from '../lib/next.ts'

  let { request, me }: { request: AppAuthorize; me: User } = $props()
  let busy = $state(false)
  let error = $state('')

  async function allow() {
    busy = true
    error = ''
    try {
      const r = await fetch('/api/v1/auth/app/approve', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(request),
      })
      if (!r.ok) throw new ApiError(r.status, `${r.status}`)
      const { redirect } = (await r.json()) as { redirect: string }
      location.replace(redirect)
    } catch (err) {
      busy = false
      error = err instanceof ApiError && err.status === 400 ? 'The app sent an invalid request; start the sign-in again from the app.' : `Something went wrong (${String(err)}).`
    }
  }
  function cancel() {
    const url = new URL(request.redirect_uri)
    url.searchParams.set('error', 'access_denied')
    url.searchParams.set('state', request.state)
    location.replace(url.href)
  }
</script>

<main class="authorize">
  <div class="card">
    <h1>Lemmate</h1>
    <p>Allow <strong>{request.device}</strong> to use your account, <strong>{me.email}</strong>?</p>
    <p class="muted">
      It gets an access token that can do whatever you can on this server. It shows under Account → Access tokens, where you can
      revoke it at any time.
    </p>
    {#if error}<p class="error">{error}</p>{/if}
    <div class="row">
      <button onclick={cancel} disabled={busy}>Cancel</button>
      <button class="primary" onclick={allow} disabled={busy}>{busy ? 'Allowing…' : 'Allow'}</button>
    </div>
  </div>
</main>

<style>
  .authorize { display: grid; place-items: center; height: 100%; }
  .card { width: min(24rem, 90vw); display: flex; flex-direction: column; gap: 0.7rem; background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 1.5rem; }
  h1 { margin: 0; }
  p { margin: 0; line-height: 1.45; }
  .muted { color: var(--muted); font-size: 0.85rem; }
  .error { color: #dc2626; font-size: 0.85rem; }
  .row { display: flex; gap: 0.5rem; justify-content: flex-end; }
  button { font: inherit; border: 1px solid var(--border); background: var(--bg); color: inherit; border-radius: 6px; padding: 0.5rem 1rem; cursor: pointer; }
  button.primary { background: var(--accent); color: white; border-color: transparent; }
  button:disabled { opacity: 0.6; cursor: default; }
</style>
