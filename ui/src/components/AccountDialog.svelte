<script lang="ts">
  // Account settings (SPEC §11.1): change your own password, and — for an admin — reset someone
  // else's and hand out single-use registration links. There is no mail on a self-hosted server,
  // so the admin reset *is* the password recovery story and the invite link is the only way in
  // when registration is closed. And personal access tokens, for the CLI, scripts and MCP — and
  // for the desktop app, on a server that signs in only through an identity provider.
  import { api, ApiError, type AccessToken, type AuthConfig, type Invite, type User } from '../lib/api.ts'

  let { me, vaults, onClose }: { me: User; vaults: { id: string; label: string }[]; onClose: () => void } = $props()

  let config = $state<AuthConfig | null>(null)
  $effect(() => {
    api.authConfig().then((c) => (config = c), () => {})
  })
  let passwords = $derived(config?.password_login ?? true)

  let tokens: AccessToken[] = $state([])
  let tokName = $state('')
  /** Empty = every vault. */
  let tokVaults: string[] = $state([])
  let tokReadOnly = $state(false)
  let tokDays = $state('')
  let tokNew = $state('')
  let tokError = $state('')
  async function reloadTokens() {
    try {
      tokens = await api.tokens()
      tokError = ''
    } catch (err) {
      tokError = String(err)
    }
  }
  $effect(() => {
    reloadTokens()
  })
  async function mintToken(e: Event) {
    e.preventDefault()
    tokError = ''
    try {
      const days = Number.parseInt(tokDays, 10)
      const t = await api.createToken({
        name: tokName.trim(),
        vaults: tokVaults.length ? tokVaults : null,
        read_only: tokReadOnly,
        expires_days: Number.isFinite(days) && days > 0 ? days : undefined,
      })
      tokNew = t.token ?? ''
      tokName = ''
      reloadTokens()
    } catch (err) {
      tokError = err instanceof ApiError && err.status === 400 ? 'Give the token a name.' : String(err)
    }
  }
  async function revokeToken(id: string) {
    try {
      await api.revokeToken(id)
      reloadTokens()
    } catch (err) {
      tokError = String(err)
    }
  }
  const vaultLabel = (id: string) => vaults.find((v) => v.id === id)?.label ?? id.slice(-6)

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
  async function copy(text = newLink) {
    try {
      await navigator.clipboard.writeText(text)
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

    {#if passwords}
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
    {:else if config?.oidc}
      <p class="muted">You sign in through {config.oidc}; your password lives there.</p>
    {/if}

    <form onsubmit={mintToken}>
      <h3>Access tokens</h3>
      <p class="muted">
        For <code>lemmate login --token</code>, scripts, MCP and the desktop app. A token can be limited to some vaults and to
        reading; it never has admin rights and cannot make more tokens. Changing your password leaves tokens alone — revoke them here.
      </p>
      <div class="row">
        <input bind:value={tokName} placeholder="name, e.g. laptop CLI" required maxlength="100" />
        <input class="days" bind:value={tokDays} type="number" min="1" placeholder="days" title="Expires after this many days (optional)" />
      </div>
      {#if vaults.length > 1}
        <div class="vaults">
          <span class="muted">Vaults:</span>
          {#each vaults as v (v.id)}
            <label class="check"><input type="checkbox" value={v.id} bind:group={tokVaults} /> {v.label}</label>
          {/each}
          <span class="muted">{tokVaults.length ? '' : '(none ticked = all)'}</span>
        </div>
      {/if}
      <div class="row">
        <label class="check"><input type="checkbox" bind:checked={tokReadOnly} /> Read only</label>
        <span class="grow"></span>
        <button type="submit">Create token</button>
      </div>
      {#if tokError}<p class="error">{tokError}</p>{/if}
      {#if tokNew}
        <div class="row"><input readonly value={tokNew} class="mono" /><button type="button" onclick={() => copy(tokNew)}>Copy</button></div>
        <p class="muted">Shown only now. Treat it like a password.</p>
      {/if}
      <ul>
        {#each tokens as t (t.id)}
          <li>
            <span>{t.name}</span>
            <span class="muted">
              {t.vaults ? t.vaults.map(vaultLabel).join(', ') : 'all vaults'}{t.read_only ? ' · read only' : ''}
              · {t.last_used_ms ? `used ${when(t.last_used_ms)}` : 'never used'}{#if t.expires_ms} · {t.expires_ms < Date.now() ? 'expired' : `expires ${when(t.expires_ms)}`}{/if}
            </span>
            <button type="button" onclick={() => revokeToken(t.id)}>Revoke</button>
          </li>
        {/each}
      </ul>
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
          <div class="row"><input readonly value={newLink} /><button type="button" onclick={() => copy()}>Copy</button></div>
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
  .grow { flex: 1; }
  label.check { flex-direction: row; align-items: center; gap: 0.35rem; }
  label.check input { flex: none; }
  input.days { flex: none; width: 5.5rem; }
  .vaults { display: flex; flex-wrap: wrap; gap: 0.3rem 0.8rem; align-items: center; }
  code { font-family: ui-monospace, monospace; font-size: 0.85em; }
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

  /* A phone has no room to spare above an overlay, and a long one must be able to scroll. */
  @media (max-width: 720px) {
    .backdrop {
      padding-top: 4vh;
    }
  }
</style>
