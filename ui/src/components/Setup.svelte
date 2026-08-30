<script lang="ts">
  // First-run setup for the desktop app (SPEC §14): the relay runs in setup mode until the
  // shell has a vault directory, a server, and (if the server has accounts) a session.
  import { untrack } from 'svelte'
  let { status, onDone }: { status: { config_path: string; suggested_vault_dir: string }; onDone: () => void } = $props()
  let vaultDir = $state(untrack(() => status.suggested_vault_dir))
  let serverUrl = $state('')
  let vaultId = $state('')
  let email = $state('')
  let password = $state('')
  let register = $state(false)
  let invite = $state('')
  let error = $state('')
  let busy = $state(false)

  async function submit(e: Event) {
    e.preventDefault()
    busy = true
    error = ''
    try {
      const r = await fetch('/api/v1/local/setup', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          vault_dir: vaultDir.trim(),
          server_url: serverUrl.trim().replace(/\/+$/u, ''),
          vault_id: vaultId.trim() || null,
          email: email.trim() || null,
          password: password || null,
          register,
          invite: invite.trim() || null,
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
    <p class="muted">Where your notes live on this computer, and which server to sync with. Saved to <code>{status.config_path}</code>.</p>
    <label>Vault folder <input bind:value={vaultDir} required /></label>
    <label>Server URL <input bind:value={serverUrl} placeholder="https://notes.example.org" required /></label>
    <label>Vault id <span class="hint">(leave empty to create a new vault)</span> <input bind:value={vaultId} placeholder="01ARZ3…" /></label>
    <fieldset>
      <legend>Account on the server</legend>
      <label>Email <input type="email" bind:value={email} autocomplete="username" /></label>
      <label>Password <input type="password" bind:value={password} autocomplete="current-password" /></label>
      <label class="check"><input type="checkbox" bind:checked={register} /> Create this account (first account on a new server)</label>
      {#if register}
        <label>Invite <input bind:value={invite} placeholder="paste the invite link, if you were sent one" /></label>
      {/if}
      <p class="hint">Leave empty for a server started with <code>--no-auth</code>.</p>
    </fieldset>
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
