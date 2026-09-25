<script lang="ts">
  // Giving a standalone app a server (SPEC §3.2). The relay carries this to the shell, which
  // signs in, writes the configuration file and restarts the app onto it — so a success here
  // ends with the window reloading, not with anything to do.
  //
  // Every vault on this machine keeps its own identity: a vault nobody owns is claimed by the
  // account that syncs it. Folding one into a vault the server already has is a different
  // thing, and is not built.
  let { configPath, onClose }: { configPath: string; onClose: () => void } = $props()

  let serverUrl = $state('')
  let email = $state('')
  let password = $state('')
  let register = $state(false)
  let invite = $state('')
  let token = $state('')
  /** The desktop shell's sign-in window (`sign_in_window` in crates/desktop), where there is one. */
  const shell = (window as unknown as { lemmateShell?: { signIn?: (server: string, ca: string) => void } }).lemmateShell
  let signedInAs = $state('')
  /** Sign in through the server's own page — its identity provider, if it has one — and go on
   *  with the token that leaves behind, as if it had been pasted. */
  function browserSignIn() {
    const server = serverUrl.trim().replace(/\/+$/u, '')
    if (!server) {
      error = 'Enter the server URL first.'
      return
    }
    error = ''
    busy = true
    const onResult = (e: Event) => {
      window.removeEventListener('lemmate-sign-in', onResult)
      const d = (e as CustomEvent<{ ok: boolean; email?: string; error?: string }>).detail
      if (d.ok) {
        signedInAs = d.email ?? ''
        void submit()
      } else {
        busy = false
        error = d.error ?? 'Sign-in failed.'
      }
    }
    window.addEventListener('lemmate-sign-in', onResult)
    shell?.signIn?.(server, caCert.trim())
  }
  let caCert = $state('')
  let error = $state('')
  let busy = $state(false)
  let done = $state(false)

  async function submit(e?: Event) {
    e?.preventDefault()
    busy = true
    error = ''
    try {
      const r = await fetch('/api/v1/local/connect', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          server_url: serverUrl.trim().replace(/\/+$/u, ''),
          ca_cert: caCert.trim() || null,
          email: email.trim() || null,
          password: password || null,
          register,
          invite: invite.trim() || null,
          token: token.trim() || null,
        }),
      })
      if (!r.ok) throw new Error((await r.text()).trim() || `${r.status} ${r.statusText}`)
      // The shell answered before restarting; the window is about to reload under us.
      done = true
    } catch (err) {
      error = String(err instanceof Error ? err.message : err)
      busy = false
    }
  }
</script>

<div class="backdrop" onmousedown={() => (busy ? null : onClose())} role="presentation">
  <div class="dialog" onmousedown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" aria-label="Connect a server" tabindex="-1">
    <h2>Connect a server</h2>
    {#if done}
      <p>Connected. Lemmate is restarting to start syncing.</p>
      <p class="muted">Your vaults keep the notes and history they already have — they become vaults on the server, one for one.</p>
    {:else}
      <p class="muted">
        Your notes stay where they are. Each vault on this computer becomes a vault on the server, with its history, and syncs from
        then on. Saved to <code>{configPath}</code>.
      </p>
      <form onsubmit={submit}>
        <label>Server URL <input bind:value={serverUrl} placeholder="https://notes.example.org" required /></label>
        <fieldset>
          <legend>Account on the server</legend>
          {#if shell?.signIn}
            <button type="button" class="primary" onclick={browserSignIn} disabled={busy}>Sign in with your browser</button>
            {#if signedInAs}<p class="hint">Signed in as {signedInAs}.</p>{/if}
            <p class="hint">Opens the server’s sign-in page — its identity provider, if it has one — and brings a token back. Or:</p>
          {/if}
          <label>Email <input type="email" bind:value={email} autocomplete="username" /></label>
          <label>Password <input type="password" bind:value={password} autocomplete="current-password" /></label>
          <label class="check"><input type="checkbox" bind:checked={register} /> Create this account (first account on a new server)</label>
          {#if register}
            <label>Invite <input bind:value={invite} placeholder="paste the invite link, if you were sent one" /></label>
          {/if}
          <label>Or an access token <span class="hint">(Account → Access tokens on the server; for one that signs in through an identity provider)</span>
            <input bind:value={token} placeholder="lmt_…" autocomplete="off" spellcheck="false" />
          </label>
          <p class="hint">Leave empty for a server started with <code>--no-auth</code>, or if you have already signed in with <code>lemmate login</code>.</p>
        </fieldset>
        <label>Private CA <span class="hint">(optional)</span> <input bind:value={caCert} placeholder="/etc/ssl/private-ca.pem" /></label>
        {#if error}<p class="error">{error}</p>{/if}
        <div class="row end">
          <button type="button" onclick={onClose} disabled={busy}>Cancel</button>
          <button class="primary" type="submit" disabled={busy}>{busy ? 'Connecting…' : 'Connect'}</button>
        </div>
      </form>
    {/if}
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgb(0 0 0 / 0.3); display: flex; align-items: flex-start; justify-content: center; padding-top: 12vh; z-index: 10; overflow: auto; }
  .dialog { width: min(34rem, 92vw); background: var(--panel); border: 1px solid var(--border); border-radius: 10px; box-shadow: 0 10px 40px rgb(0 0 0 / 0.3); padding: 1rem 1.2rem; display: flex; flex-direction: column; gap: 0.6rem; }
  h2 { margin: 0; font-size: 1.05rem; }
  form { display: flex; flex-direction: column; gap: 0.6rem; }
  label { display: flex; flex-direction: column; gap: 0.2rem; font-size: 0.85rem; color: var(--muted); }
  label.check { flex-direction: row; align-items: center; gap: 0.5rem; }
  input:not([type='checkbox']) { font: inherit; padding: 0.4rem 0.55rem; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: inherit; }
  fieldset { border: 1px solid var(--border); border-radius: 8px; display: flex; flex-direction: column; gap: 0.5rem; }
  legend { font-size: 0.85rem; color: var(--muted); padding: 0 0.3rem; }
  .row { display: flex; gap: 0.4rem; align-items: center; }
  .row.end { justify-content: flex-end; }
  button { font: inherit; font-size: 0.9rem; border: 1px solid var(--border); background: var(--bg); color: inherit; border-radius: 6px; padding: 0.35rem 0.7rem; cursor: pointer; }
  button.primary { border-color: transparent; background: var(--accent); color: white; }
  button:disabled { opacity: 0.6; cursor: default; }
  p { margin: 0; }
  .muted, .hint { color: var(--muted); font-size: 0.85rem; }
  .error { color: #dc2626; font-size: 0.85rem; white-space: pre-wrap; }
  code { font-family: var(--mono); font-size: 0.85em; }

  /* A phone has no room to spare above an overlay, and a long one must be able to scroll. */
  @media (max-width: 720px) {
    .backdrop { padding-top: 5vh; }
  }
</style>
