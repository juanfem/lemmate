<script lang="ts">
  import { untrack } from 'svelte'
  import { api, ApiError, type AuthConfig } from '../lib/api.ts'
  import { appAuthorize, renderReturn } from '../lib/next.ts'

  let {
    onDone,
    invite = null,
    stay = false,
  }: {
    onDone: () => void
    invite?: string | null
    /** Just signed out: offer the provider, do not go to it (its session would sign us back in). */
    stay?: boolean
  } = $props()
  // What the server offers: passwords, an identity provider, or both. Until it answers, assume
  // passwords, which is what every server without OIDC does.
  let config = $state<AuthConfig | null>(null)
  // Nothing is drawn until the server has said what it offers, so a provider-only server never
  // flashes a password form on its way to the provider.
  let loaded = $state(false)
  $effect(() => {
    api.authConfig().then(
      (c) => ((config = c), (loaded = true)),
      () => (loaded = true),
    )
  })
  let passwords = $derived(config?.password_login ?? true)
  // A render opened without a session comes back to itself after the round trip, and so does an
  // app's sign-in request (`?authorize=app…`), which is this very page.
  const next = renderReturn(location.search) ?? (appAuthorize(location.search) ? location.pathname + location.search : null)
  let oidcHref = $derived.by(() => {
    const q = new URLSearchParams()
    if (invite) q.set('invite', invite)
    if (next) q.set('next', next)
    return `/api/v1/auth/oidc/start${q.size ? `?${q}` : ''}`
  })
  // A sign-in through the identity provider that failed comes back as `?signin_error=…`: show
  // it once, and take it out of the address bar so a reload does not show it again.
  const signinError = new URLSearchParams(location.search).get('signin_error')
  if (signinError) {
    const url = new URL(location.href)
    url.searchParams.delete('signin_error')
    history.replaceState(history.state, '', url)
  }
  // Arriving on an invite link means the point is to create an account, so start there.
  let mode: 'login' | 'register' = $state(untrack(() => (invite ? 'register' : 'login')))
  let email = $state('')
  let password = $state('')
  let name = $state('')
  let error = $state(untrack(() => signinError ?? ''))
  // When the identity provider is the only way in, this page has nothing to ask: go straight
  // there. Not after a failed attempt, though — that would loop, and the reason would never show.
  let redirecting = $derived(!!config?.oidc && !config.password_login && !signinError && !stay)
  $effect(() => {
    if (redirecting) location.replace(oidcHref)
  })
  let busy = $state(false)

  async function submit(e: Event) {
    e.preventDefault()
    busy = true
    error = ''
    try {
      if (mode === 'login') await api.login(email, password)
      else {
        const r = await api.register(email, password, name || email.split('@')[0] || 'me', invite ?? undefined)
        if (!r.token) await api.login(email, password)
      }
      onDone()
    } catch (err) {
      const status = err instanceof ApiError ? err.status : 0
      error =
        status === 401 ? 'Wrong email or password.'
        : status === 403 && invite ? 'This invite has already been used, expired, or was revoked. Ask the admin for a new link.'
        : status === 403 ? 'Registration is closed on this server; ask the admin for an account.'
        : status === 409 ? 'An account with that email already exists.'
        : status === 400 ? 'Use a valid email and a password of at least 8 characters.'
        : `Something went wrong (${status || 'network'}).`
    } finally {
      busy = false
    }
  }
</script>

<main class="login">
  <form onsubmit={submit}>
    <h1>Lemmate</h1>
    {#if !loaded || redirecting}
      <p class="muted">{redirecting ? `Signing in with ${config?.oidc}…` : 'Loading…'}</p>
    {:else if config?.oidc}
      {#if stay && !passwords}<p class="muted">You are signed out.</p>{/if}
      <a class="primary sso" href={oidcHref}>{invite ? 'Accept the invite with' : 'Sign in with'} {config.oidc}</a>
      {#if !passwords}
        <p class="muted">{invite ? 'The invite creates your account the first time you sign in; it works once.' : 'This server signs in through your identity provider.'}</p>
        {#if error}<p class="error">{error}</p>{/if}
      {:else}
        <p class="or">or with a password</p>
      {/if}
    {/if}
    {#if loaded && passwords}
    <p class="muted">
      {#if mode === 'login'}Sign in to your server.
      {:else if invite}You were invited. Pick an email and a password; the link works once.
      {:else}Create an account. The first account becomes the admin.{/if}
    </p>
    {#if mode === 'register'}
      <label>Name <input bind:value={name} autocomplete="name" /></label>
    {/if}
    <label>Email <input type="email" bind:value={email} autocomplete="username" required /></label>
    <label>Password <input type="password" bind:value={password} autocomplete={mode === 'login' ? 'current-password' : 'new-password'} required minlength="8" /></label>
    {#if error}<p class="error">{error}</p>{/if}
    <button class="primary" type="submit" disabled={busy}>{mode === 'login' ? 'Sign in' : 'Create account'}</button>
    <button type="button" class="link" onclick={() => (mode = mode === 'login' ? 'register' : 'login')}>
      {mode === 'login' ? 'Need an account?' : 'Have an account? Sign in'}
    </button>
    {/if}
  </form>
</main>

<style>
  .login { display: grid; place-items: center; height: 100%; }
  form { width: min(22rem, 90vw); display: flex; flex-direction: column; gap: 0.7rem; background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 1.5rem; }
  h1 { margin: 0; }
  label { display: flex; flex-direction: column; gap: 0.2rem; font-size: 0.85rem; color: var(--muted); }
  input { font: inherit; padding: 0.45rem 0.6rem; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: inherit; }
  .primary { font: inherit; background: var(--accent); color: white; border: 0; border-radius: 6px; padding: 0.55rem 1rem; cursor: pointer; }
  .primary:disabled { opacity: 0.6; }
  a.sso { text-align: center; text-decoration: none; }
  .or { margin: 0; text-align: center; font-size: 0.8rem; color: var(--muted); }
  .link { font: inherit; font-size: 0.85rem; background: none; border: 0; color: var(--accent); cursor: pointer; }
  .muted { color: var(--muted); margin: 0; font-size: 0.9rem; }
  .error { color: #dc2626; margin: 0; font-size: 0.85rem; }
</style>
