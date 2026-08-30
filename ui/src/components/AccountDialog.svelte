<script lang="ts">
  // Account settings (SPEC §11.1): change your own password, and — for an admin — reset someone
  // else's and hand out single-use registration links. There is no mail on a self-hosted server,
  // so the admin reset *is* the password recovery story and the invite link is the only way in
  // when registration is closed.
  import { api, ApiError, type Invite, type User } from '../lib/api.ts'

  let { me, onClose }: { me: User; onClose: () => void } = $props()

  let current = $state('')
  let next = $state('')
  let repeat = $state('')
  let resetEmail = $state('')
  let pwError = $state('')
  let pwDone = $state('')
  let busy = $state(false)

  let invites: Invite[] = $state([])
  let newLink = $state('')
  let expiresDays = $state('')
  let invError = $state('')

  /** Resetting another account is the admin path and never asks for the current password. */
  let resetting = $derived(resetEmail.trim() !== '' && resetEmail.trim().toLowerCase() !== me.email.toLowerCase())

  async function submitPassword(e: Event) {
    e.preventDefault()
    pwError = ''
    pwDone = ''
    if (next !== repeat) {
      pwError = 'The two new passwords do not match.'
      return
    }
    busy = true
    try {
      const target = resetting ? resetEmail.trim() : undefined
      const r = await api.changePassword(next, resetting ? undefined : current, target)
      pwDone = resetting
        ? `Password reset for ${target}. ${r.sessions_revoked} session(s) signed out; tell them to sign in again.`
        : `Password changed. ${r.sessions_revoked} other session(s) signed out; this one is still valid.`
      current = ''
      next = ''
      repeat = ''
    } catch (err) {
      const status = err instanceof ApiError ? err.status : 0
      pwError =
        status === 401 ? 'That is not your current password.'
        : status === 403 ? 'Only an admin can reset another account.'
        : status === 404 ? 'No account with that email on this server.'
        : status === 400 ? 'The new password must be at least 8 characters.'
        : `Something went wrong (${status || 'network'}).`
    } finally {
      busy = false
    }
  }

  async function reloadInvites() {
    if (!me.is_admin) return
    try {
      invites = await api.invites()
      invError = ''
    } catch (err) {
      invError = String(err)
    }
  }
  $effect(() => {
    reloadInvites()
  })

  async function mint() {
    invError = ''
    try {
      const days = Number.parseInt(expiresDays, 10)
      const i = await api.createInvite(Number.isFinite(days) && days > 0 ? days : undefined)
      newLink = i.link ? `${location.origin}${i.link}` : ''
      reloadInvites()
    } catch (err) {
      invError = String(err)
    }
  }
  async function copy() {
    try {
      await navigator.clipboard.writeText(newLink)
    } catch {
      /* clipboard may be unavailable; the link is on screen to copy by hand */
    }
  }
  async function revoke(id: string) {
    try {
      await api.revokeInvite(id)
      reloadInvites()
    } catch (err) {
      invError = err instanceof ApiError && err.status === 409 ? 'That invite has already been used; it is kept as a record.' : String(err)
    }
  }
  function when(ms: number | null): string {
    return ms ? new Date(ms).toLocaleDateString() : ''
  }
</script>

<div class="backdrop" onmousedown={onClose} role="presentation">
  <div class="dialog" onmousedown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" aria-label="Account" tabindex="-1">
    <h2>Account · {me.email}</h2>

    <form onsubmit={submitPassword}>
      <h3>{resetting ? 'Reset another account' : 'Change your password'}</h3>
      {#if me.is_admin}
        <label>Reset for <input bind:value={resetEmail} type="email" placeholder="leave empty for your own account" /></label>
      {/if}
      {#if !resetting}
        <label>Current password <input bind:value={current} type="password" autocomplete="current-password" required /></label>
      {/if}
      <label>New password <input bind:value={next} type="password" autocomplete="new-password" required minlength="8" /></label>
      <label>Repeat <input bind:value={repeat} type="password" autocomplete="new-password" required minlength="8" /></label>
      {#if pwError}<p class="error">{pwError}</p>{/if}
      {#if pwDone}<p class="ok">{pwDone}</p>{/if}
      <div class="row end"><button class="primary" type="submit" disabled={busy}>{resetting ? 'Reset password' : 'Change password'}</button></div>
      <p class="muted">Every other session of that account is signed out, so any other device has to sign in again.</p>
    </form>

    {#if me.is_admin}
      <form onsubmit={(e) => { e.preventDefault(); mint() }}>
        <h3>Invite someone</h3>
        <div class="row">
          <input bind:value={expiresDays} type="number" min="1" placeholder="expires in days (optional)" />
          <button type="submit">Create invite link</button>
        </div>
        {#if invError}<p class="error">{invError}</p>{/if}
        {#if newLink}
          <div class="row"><input readonly value={newLink} /><button type="button" onclick={copy}>Copy</button></div>
          <p class="muted">Send this however you like. It creates exactly one account and then stops working — and it is shown only now.</p>
        {/if}
        <ul>
          {#each invites as i (i.id)}
            <li>
              <span class="mono">{i.id.slice(0, 12)}…</span>
              <span class="muted">
                {#if i.used_by}used by {i.used_by}{:else if i.usable}unused{#if i.expires_ms}, expires {when(i.expires_ms)}{/if}{:else}expired{/if}
              </span>
              {#if !i.used_by}<button type="button" onclick={() => revoke(i.id)}>Revoke</button>{/if}
            </li>
          {/each}
        </ul>
      </form>
    {/if}

    <div class="row end"><button onclick={onClose}>Done</button></div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgb(0 0 0 / 0.3); display: flex; align-items: flex-start; justify-content: center; padding-top: 8vh; z-index: 10; overflow: auto; }
  .dialog { width: min(34rem, 92vw); background: var(--panel); border: 1px solid var(--border); border-radius: 10px; box-shadow: 0 10px 40px rgb(0 0 0 / 0.3); padding: 1rem 1.2rem; display: flex; flex-direction: column; gap: 0.8rem; }
  h2 { margin: 0; font-size: 1.05rem; }
  h3 { margin: 0 0 0.4rem; font-size: 0.9rem; color: var(--muted); font-weight: 600; }
  form { display: flex; flex-direction: column; gap: 0.4rem; border-top: 1px solid var(--border); padding-top: 0.7rem; }
  label { display: flex; flex-direction: column; gap: 0.2rem; font-size: 0.85rem; color: var(--muted); }
  .row { display: flex; gap: 0.4rem; align-items: center; }
  .row.end { justify-content: flex-end; }
  input { font: inherit; padding: 0.35rem 0.5rem; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: inherit; flex: 1; }
  button { font: inherit; font-size: 0.9rem; border: 1px solid var(--border); background: var(--bg); color: inherit; border-radius: 6px; padding: 0.35rem 0.7rem; cursor: pointer; }
  .primary { background: var(--accent); color: white; border-color: transparent; }
  .primary:disabled { opacity: 0.6; }
  ul { list-style: none; margin: 0.3rem 0 0; padding: 0; display: flex; flex-direction: column; gap: 0.2rem; }
  li { display: flex; justify-content: space-between; align-items: center; gap: 0.5rem; font-size: 0.85rem; }
  .mono { font-family: ui-monospace, monospace; }
  .muted { color: var(--muted); font-size: 0.8rem; margin: 0; }
  .error { color: #dc2626; margin: 0; font-size: 0.85rem; }
  .ok { color: #16a34a; margin: 0; font-size: 0.85rem; }
</style>
