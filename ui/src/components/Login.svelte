<script lang="ts">
  import { api, ApiError } from '../lib/api.ts'

  let { onDone }: { onDone: () => void } = $props()
  let mode: 'login' | 'register' = $state('login')
  let email = $state('')
  let password = $state('')
  let name = $state('')
  let error = $state('')
  let busy = $state(false)

  async function submit(e: Event) {
    e.preventDefault()
    busy = true
    error = ''
    try {
      if (mode === 'login') await api.login(email, password)
      else {
        const r = await api.register(email, password, name || email.split('@')[0] || 'me')
        if (!r.token) await api.login(email, password)
      }
      onDone()
    } catch (err) {
      const status = err instanceof ApiError ? err.status : 0
      error =
        status === 401 ? 'Wrong email or password.'
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
    <h1>notes</h1>
    <p class="muted">{mode === 'login' ? 'Sign in to your server.' : 'Create an account. The first account becomes the admin.'}</p>
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
  .link { font: inherit; font-size: 0.85rem; background: none; border: 0; color: var(--accent); cursor: pointer; }
  .muted { color: var(--muted); margin: 0; font-size: 0.9rem; }
  .error { color: #dc2626; margin: 0; font-size: 0.85rem; }
</style>
