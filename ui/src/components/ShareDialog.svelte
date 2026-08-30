<script lang="ts">
  import { api, ApiError, type Share } from '../lib/api.ts'

  let { vault, noteId, path, onClose }: { vault: string; noteId: string; path: string; onClose: () => void } = $props()
  let shares: Share[] = $state([])
  let email = $state('')
  let role = $state('viewer')
  let error = $state('')
  let newLink = $state('')

  async function reload() {
    try {
      shares = await api.shares(vault, noteId)
      error = ''
    } catch (e) {
      error = e instanceof ApiError && e.status === 404 ? 'Sharing is not available here (no accounts on this server).' : String(e)
    }
  }
  $effect(() => {
    reload()
  })
  async function addUser(e: Event) {
    e.preventDefault()
    try {
      await api.share(vault, noteId, { kind: 'user', email: email.trim(), role })
      email = ''
      reload()
    } catch (err) {
      error = err instanceof ApiError && err.status === 404 ? 'No account with that email on this server.' : String(err)
    }
  }
  async function makeLink() {
    try {
      const s = await api.share(vault, noteId, { kind: 'link' })
      newLink = s.link ? `${location.origin}/${s.link}` : ''
      reload()
    } catch (err) {
      error = String(err)
    }
  }
  async function copy() {
    try {
      await navigator.clipboard.writeText(newLink)
    } catch {
      /* clipboard may be unavailable; the link is visible to copy by hand */
    }
  }
  async function removeUser(id: string) {
    await api.unshare(vault, noteId, { user_id: id }).catch((e) => (error = String(e)))
    reload()
  }
  async function revokeLinks() {
    await api.unshare(vault, noteId, { links: true }).catch((e) => (error = String(e)))
    newLink = ''
    reload()
  }
  let links = $derived(shares.filter((s) => s.kind === 'link'))
  let users = $derived(shares.filter((s) => s.kind === 'user'))
</script>

<div class="backdrop" onmousedown={onClose} role="presentation">
  <div class="dialog" onmousedown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" aria-label="Share note" tabindex="-1">
    <h2>Share “{path}”</h2>
    {#if error}<p class="error">{error}</p>{/if}
    <form onsubmit={addUser} class="row">
      <input bind:value={email} type="email" placeholder="someone@example.org" required />
      <select bind:value={role}><option value="viewer">can view</option><option value="editor">can edit</option></select>
      <button type="submit">Share</button>
    </form>
    <ul>
      {#each users as s (s.user_id)}
        <li><span>{s.email} · {s.role}</span><button onclick={() => removeUser(s.user_id!)}>Remove</button></li>
      {/each}
    </ul>
    <div class="row">
      <button onclick={makeLink}>Create public read-only link</button>
      {#if links.length}<button onclick={revokeLinks}>Revoke {links.length === 1 ? 'link' : `${links.length} links`}</button>{/if}
    </div>
    {#if newLink}
      <div class="row link"><input readonly value={newLink} /><button onclick={copy}>Copy</button></div>
      <p class="muted">Anyone with this link can read the note (not edit it). Shown once; revoke to disable.</p>
    {/if}
    <div class="row end"><button onclick={onClose}>Done</button></div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgb(0 0 0 / 0.3); display: flex; align-items: flex-start; justify-content: center; padding-top: 12vh; z-index: 10; }
  .dialog { width: min(34rem, 92vw); background: var(--panel); border: 1px solid var(--border); border-radius: 10px; box-shadow: 0 10px 40px rgb(0 0 0 / 0.3); padding: 1rem 1.2rem; display: flex; flex-direction: column; gap: 0.6rem; }
  h2 { margin: 0; font-size: 1.05rem; }
  .row { display: flex; gap: 0.4rem; align-items: center; }
  .row.end { justify-content: flex-end; }
  input, select { font: inherit; padding: 0.35rem 0.5rem; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: inherit; flex: 1; }
  button { font: inherit; font-size: 0.9rem; border: 1px solid var(--border); background: var(--bg); color: inherit; border-radius: 6px; padding: 0.35rem 0.7rem; cursor: pointer; }
  ul { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 0.2rem; }
  li { display: flex; justify-content: space-between; align-items: center; font-size: 0.9rem; }
  .muted { color: var(--muted); font-size: 0.8rem; margin: 0; }
  .error { color: #dc2626; margin: 0; font-size: 0.85rem; }
</style>
