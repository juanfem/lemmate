<script lang="ts">
  // The desktop app after signing out (crates/desktop, `watch_for_sign_out`), or with a token the
  // server no longer accepts: the relay answers /api/v1/auth/me with 401. The notes are still in
  // their folders; what is missing is an account to sync them as. Signing in again goes through
  // the server's page in a window of the shell's (`sign_in_window`), then asks the shell to
  // reconnect — the same request *Connect a server…* makes, which restarts the app signed in.
  import ConnectServer from './ConnectServer.svelte'

  let {
    server,
    caCert,
    configPath,
    canConnect,
  }: { server: string; caCert: string; configPath: string; /** A shell that can reconnect (the desktop app). */ canConnect: boolean } = $props()
  const shell = (window as unknown as { lemmateShell?: { signIn?: (server: string, ca: string) => void } }).lemmateShell
  let busy = $state(false)
  let done = $state(false)
  let error = $state('')
  let other = $state(false)

  async function reconnect() {
    const r = await fetch('/api/v1/local/connect', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ server_url: server, ca_cert: caCert || null }),
    })
    if (!r.ok) throw new Error((await r.text()).trim() || `${r.status} ${r.statusText}`)
    done = true
  }
  function signIn() {
    busy = true
    error = ''
    const onResult = (e: Event) => {
      window.removeEventListener('lemmate-sign-in', onResult)
      const d = (e as CustomEvent<{ ok: boolean; error?: string }>).detail
      if (!d.ok) {
        busy = false
        error = d.error ?? 'Sign-in failed.'
        return
      }
      reconnect().catch((err) => {
        busy = false
        error = String(err instanceof Error ? err.message : err)
      })
    }
    window.addEventListener('lemmate-sign-in', onResult)
    shell?.signIn?.(server, caCert)
  }
</script>

<main class="signed-out">
  <div class="card">
    <h1>Lemmate</h1>
    {#if done}
      <p>Signed in. Lemmate is restarting to sync again.</p>
    {:else}
      <p>Signed out of <strong>{server}</strong>.</p>
      <p class="muted">Your notes are still in their folders on this computer. Sign in to sync them again.</p>
      {#if error}<p class="error">{error}</p>{/if}
      {#if !canConnect}
        <!-- `lemmate serve` / `sync --serve`: signed in by its flags, so the terminal signs it in. -->
        <p class="muted">Run <code>lemmate login --server {server} --browser</code>, then start this again.</p>
      {:else}
        {#if shell?.signIn}
          <button class="primary" onclick={signIn} disabled={busy}>{busy ? 'Waiting for the sign-in window…' : 'Sign in with your browser'}</button>
        {/if}
        <button class="link" onclick={() => (other = true)} disabled={busy}>Other ways to sign in…</button>
      {/if}
    {/if}
  </div>
</main>

{#if other}
  <ConnectServer {configPath} initialServer={server} initialCa={caCert} onClose={() => (other = false)} />
{/if}

<style>
  .signed-out { display: grid; place-items: center; height: 100%; }
  .card { width: min(24rem, 90vw); display: flex; flex-direction: column; gap: 0.7rem; background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 1.5rem; }
  h1 { margin: 0; }
  p { margin: 0; line-height: 1.45; }
  .muted { color: var(--muted); font-size: 0.9rem; }
  .error { color: #dc2626; font-size: 0.85rem; white-space: pre-wrap; }
  .primary { font: inherit; background: var(--accent); color: white; border: 0; border-radius: 6px; padding: 0.55rem 1rem; cursor: pointer; }
  .primary:disabled { opacity: 0.6; cursor: default; }
  code { font-family: var(--mono); font-size: 0.85em; word-break: break-all; }
  .link { font: inherit; font-size: 0.85rem; background: none; border: 0; color: var(--accent); cursor: pointer; }
</style>
