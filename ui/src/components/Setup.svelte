<script lang="ts">
  // First-run setup for the desktop app (SPEC §14): the relay runs in setup mode until the
  // shell has a folder for the notes and — only if the notes are to sync — a server and (if
  // that server has accounts) a session.
  //
  // A server is optional: with none the app is standalone (SPEC §3.2), the vaults are folders
  // on this machine and nothing goes on the wire. One can be added later by editing the config
  // file the path below names.
  //
  // No vault is named here. With a server the shell opens every vault the account can read, one
  // folder each under the folder below — the same workspace the web client shows (SPEC §9).
  import { untrack } from 'svelte'
  let { status, onDone }: { status: { config_path: string; suggested_root_dir: string }; onDone: () => void } = $props()
  let rootDir = $state(untrack(() => status.suggested_root_dir))
  let sync = $state(false)
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
    shell?.signIn?.(server, '')
  }
  let error = $state('')
  let busy = $state(false)

  async function submit(e?: Event) {
    e?.preventDefault()
    busy = true
    error = ''
    try {
      const r = await fetch('/api/v1/local/setup', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          root_dir: rootDir.trim(),
          // Standalone: no server, and with it no account to sign in to.
          server_url: sync ? serverUrl.trim().replace(/\/+$/u, '') : null,
          email: sync ? email.trim() || null : null,
          password: sync ? password || null : null,
          register: sync && register,
          invite: sync ? invite.trim() || null : null,
          token: sync ? token.trim() || null : null,
        }),
      })
      if (!r.ok) throw new Error(`${r.status}`)
      // The desktop shell now writes the config, signs in, starts the relay and navigates
      // this window; if that takes long, keep showing the busy state.
      onDone()
    } catch (err) {
      error = `Setup failed (${String(err)}). Check the values and try again.`
      busy = false
    }
  }
</script>

<main class="setup">
  <form onsubmit={submit}>
    <h1>Set up notes</h1>
    <p class="muted">Where your notes live on this computer, and whether they sync anywhere. Saved to <code>{status.config_path}</code>.</p>
    <label>Notes folder <span class="hint">(one folder per vault goes in here)</span> <input bind:value={rootDir} required /></label>
    <label class="check"><input type="checkbox" bind:checked={sync} /> Sync with a server</label>
    {#if sync}
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
        <p class="hint">Leave empty for a server started with <code>--no-auth</code>.</p>
      </fieldset>
    {:else}
      <p class="hint">Your notes stay on this computer — no server, no account, nothing on the network. You can add a server later.</p>
    {/if}
    {#if error}<p class="error">{error}</p>{/if}
    <button class="primary" type="submit" disabled={busy}>{busy ? 'Starting…' : 'Start'}</button>
  </form>
</main>

<style>
  .setup { display: grid; place-items: center; height: 100%; overflow: auto; }
  form { width: min(30rem, 92vw); display: flex; flex-direction: column; gap: 0.7rem; background: var(--panel); border: 1px solid var(--border); border-radius: 10px; padding: 1.5rem; margin: 2rem 0; }
  h1 { margin: 0; }
  label { display: flex; flex-direction: column; gap: 0.2rem; font-size: 0.85rem; color: var(--muted); }
  label.check { flex-direction: row; align-items: center; gap: 0.5rem; }
  input:not([type='checkbox']) { font: inherit; padding: 0.45rem 0.6rem; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: inherit; }
  fieldset { border: 1px solid var(--border); border-radius: 8px; display: flex; flex-direction: column; gap: 0.5rem; }
  legend { font-size: 0.85rem; color: var(--muted); padding: 0 0.3rem; }
  .primary { font: inherit; background: var(--accent); color: white; border: 0; border-radius: 6px; padding: 0.55rem 1rem; cursor: pointer; }
  .primary:disabled { opacity: 0.6; }
  .muted, .hint { color: var(--muted); margin: 0; font-size: 0.85rem; }
  .error { color: #dc2626; margin: 0; font-size: 0.85rem; }
  code { font-family: var(--mono); font-size: 0.85em; }
</style>
