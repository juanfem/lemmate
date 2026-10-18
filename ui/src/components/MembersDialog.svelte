<script lang="ts">
  import { api, ApiError } from '../lib/api.ts'
  import { trapFocus } from '../lib/focustrap.ts'

  let {
    vault,
    label,
    myId,
    onClose,
  }: {
    vault: string
    label: string
    /** Who is looking: only an owner may add, change or remove anyone. */
    myId: string
    onClose: () => void
  } = $props()

  type Member = Awaited<ReturnType<typeof api.members>>[number]
  let members: Member[] = $state([])
  let loaded = $state(false)
  let email = $state('')
  let role = $state('editor')
  let error = $state('')

  let owner = $derived(members.some((m) => m.user_id === myId && m.role === 'owner'))

  /** What the server's refusals mean here (`put_member` / `delete_member` in auth.rs). */
  function explain(e: unknown, notFound: string): string {
    if (!(e instanceof ApiError)) return String(e)
    if (e.status === 404) return notFound
    if (e.status === 409) return 'A vault needs at least one owner — make someone else owner first.'
    if (e.status === 403) return 'Only an owner can change who is in this vault.'
    return String(e)
  }

  async function reload() {
    try {
      members = await api.members(vault)
    } catch (e) {
      error = explain(e, 'Members are not available here (no accounts on this server).')
    }
    loaded = true
  }
  $effect(() => {
    reload()
  })

  async function add(e: Event) {
    e.preventDefault()
    error = ''
    try {
      await api.setMember(vault, email.trim(), role)
      email = ''
      await reload()
    } catch (err) {
      error = explain(err, 'No account with that email on this server — invite them first (Account settings).')
    }
  }
  async function change(m: Member, select: HTMLSelectElement) {
    error = ''
    try {
      await api.setMember(vault, m.email, select.value)
    } catch (err) {
      error = explain(err, 'That account no longer exists.')
      select.value = m.role // refused: the list below has not changed, so neither does the select
    }
    await reload()
  }
  async function remove(m: Member) {
    error = ''
    try {
      await api.removeMember(vault, m.user_id)
    } catch (err) {
      error = explain(err, 'That member is already gone.')
    }
    await reload()
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onClose()} />

<div class="backdrop" onmousedown={onClose} role="presentation">
  <div class="dialog" use:trapFocus onmousedown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" aria-label="Vault members" tabindex="-1">
    <h2>Members of “{label}”</h2>
    {#if error}<p class="error">{error}</p>{/if}
    {#if owner}
      <form onsubmit={add} class="row">
        <input bind:value={email} type="email" placeholder="someone@example.org" required />
        <select bind:value={role}>
          <option value="viewer">can view</option>
          <option value="editor">can edit</option>
          <option value="owner">owner</option>
        </select>
        <button type="submit">Add</button>
      </form>
    {/if}
    <ul>
      {#each members as m (m.user_id)}
        <li>
          <span class="who">
            <span>{m.display_name || m.email}{#if m.user_id === myId}{' '}<span class="muted">(you)</span>{/if}</span>
            {#if m.display_name && m.display_name !== m.email}<span class="muted">{m.email}</span>{/if}
          </span>
          {#if owner}
            <select value={m.role} onchange={(e) => change(m, e.currentTarget)} aria-label={`Role of ${m.email}`}>
              <option value="viewer">can view</option>
              <option value="editor">can edit</option>
              <option value="owner">owner</option>
            </select>
            <button onclick={() => remove(m)}>Remove</button>
          {:else}
            <span class="role">{m.role === 'owner' ? 'owner' : m.role === 'editor' ? 'can edit' : 'can view'}</span>
          {/if}
        </li>
      {/each}
    </ul>
    {#if loaded && !owner && !error}<p class="muted">Only an owner can add or change members.</p>{/if}
    <p class="muted">Members see the whole vault. To give someone a single note, use <em>Share note…</em> instead.</p>
    <div class="row end"><button onclick={onClose}>Done</button></div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgb(0 0 0 / 0.3); display: flex; align-items: flex-start; justify-content: center; padding-top: 12vh; z-index: 10; overflow: auto; }
  .dialog { width: min(34rem, 92vw); background: var(--panel); border: 1px solid var(--border); border-radius: 10px; box-shadow: 0 10px 40px rgb(0 0 0 / 0.3); padding: 1rem 1.2rem; display: flex; flex-direction: column; gap: 0.6rem; }
  h2 { margin: 0; font-size: 1.05rem; }
  .row { display: flex; gap: 0.4rem; align-items: center; }
  .row.end { justify-content: flex-end; }
  .row input { flex: 1; min-width: 0; }
  input, select { font: inherit; padding: 0.35rem 0.5rem; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: inherit; }
  button { font: inherit; font-size: 0.9rem; border: 1px solid var(--border); background: var(--bg); color: inherit; border-radius: 6px; padding: 0.35rem 0.7rem; cursor: pointer; }
  ul { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 0.3rem; }
  li { display: flex; gap: 0.4rem; align-items: center; font-size: 0.9rem; }
  .who { flex: 1; min-width: 0; display: flex; flex-direction: column; overflow-wrap: anywhere; }
  .role { color: var(--muted); }
  .muted { color: var(--muted); font-size: 0.8rem; margin: 0; }
  .error { color: #dc2626; margin: 0; font-size: 0.85rem; }

  /* A phone has no room to spare above an overlay, and a long one must be able to scroll. */
  @media (max-width: 720px) {
    .backdrop {
      padding-top: 5vh;
    }
  }
</style>
