<script lang="ts">
  import { api } from '../lib/api.ts'
  import { displayName } from '../lib/vault.svelte.ts'

  let {
    vault: initial,
    vaults = [],
    version,
    onRestored,
    onBack,
  }: {
    /** The vault to show first: the open note's. */
    vault: string
    /** Every vault, when there is more than one to choose from; trash is per vault. */
    vaults?: { id: string; label: string }[]
    version: number
    onRestored: (id: string) => void
    onBack: () => void
  } = $props()
  let picked: string | null = $state(null)
  let vault = $derived(picked ?? initial)
  let items: { id: string; path: string; title: string | null; deleted_at: string }[] = $state([])
  let error = $state('')
  async function reload() {
    try {
      items = await api.trash(vault)
      error = ''
    } catch {
      items = []
      error = 'Trash is not available here.'
    }
  }
  $effect(() => {
    version
    void vault
    reload()
  })
  async function restore(id: string) {
    try {
      const n = await api.restore(vault, id)
      onRestored(n.id)
      reload()
    } catch (e) {
      error = String(e)
    }
  }
</script>

<div class="trash">
  <div class="head">
    <button class="back" onclick={onBack} title="Back to files" aria-label="Back to files">‹</button>
    <strong>Trash</strong>
    {#if vaults.length > 1}
      <select value={vault} onchange={(e) => (picked = e.currentTarget.value)} aria-label="Vault">
        {#each vaults as v (v.id)}<option value={v.id}>{v.label}</option>{/each}
      </select>
    {/if}
  </div>
  {#if error}<p class="muted">{error}</p>{/if}
  <ul>
    {#each items as n (n.id)}
      <li>
        <span><strong>{n.title ?? displayName(n.path)}</strong><br /><span class="muted">{n.path} · {new Date(n.deleted_at).toLocaleString()}</span></span>
        <button onclick={() => restore(n.id)}>Restore</button>
      </li>
    {/each}
    {#if items.length === 0 && !error}<li class="muted">Trash is empty. Deleted notes wait here until you restore them or the server purges them.</li>{/if}
  </ul>
</div>

<style>
  .trash { overflow: auto; font-size: 0.85rem; }
  .head { display: flex; align-items: center; gap: 0.4rem; padding: 0.35rem 0.4rem; border-bottom: 1px solid var(--border); }
  .head .back { border: 0; background: none; color: var(--muted); font-size: 1.1rem; line-height: 1; padding: 0 0.3rem; }
  .head .back:hover { color: var(--fg); background: var(--hover); }
  select { margin-left: auto; min-width: 0; font: inherit; font-size: 0.8rem; color: inherit; background: var(--bg); border: 1px solid var(--border); border-radius: 6px; padding: 0.1rem 0.3rem; }
  ul { list-style: none; margin: 0; padding: 0.4rem; display: flex; flex-direction: column; gap: 0.4rem; }
  li { display: flex; justify-content: space-between; align-items: center; gap: 0.5rem; }
  button { font: inherit; font-size: 0.8rem; border: 1px solid var(--border); background: var(--bg); color: inherit; border-radius: 6px; padding: 0.2rem 0.6rem; cursor: pointer; }
  .muted { color: var(--muted); }

  /* Touch: a list row is a target, not a line of text. */
  @media (pointer: coarse) {
    button {
      padding-top: 0.5rem;
      padding-bottom: 0.5rem;
    }
  }
</style>
