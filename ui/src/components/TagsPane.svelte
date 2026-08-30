<script lang="ts">
  import { api, type NoteSummary } from '../lib/api.ts'
  import { displayName } from '../lib/vault.svelte.ts'

  let { vault, version, onOpen }: { vault: string; version: number; onOpen: (id: string) => void } = $props()
  let tags: { tag: string; count: number }[] = $state([])
  let selected: string | null = $state(null)
  let notes: NoteSummary[] = $state([])

  $effect(() => {
    version // reload whenever the vault changes
    api.tags(vault).then((t) => (tags = t)).catch(() => (tags = []))
  })
  async function pick(tag: string) {
    selected = selected === tag ? null : tag
    notes = selected ? await api.tagged(vault, selected).catch(() => []) : []
  }
</script>

<div class="tags">
  <div class="cloud">
    {#each tags as t (t.tag)}
      <button class="chip" class:on={t.tag === selected} onclick={() => pick(t.tag)}>#{t.tag} <span>{t.count}</span></button>
    {/each}
    {#if tags.length === 0}<p class="empty">No tags yet. Type <code>#tag</code> in a note.</p>{/if}
  </div>
  {#if selected}
    <ul>
      {#each notes as n (n.id)}
        <li><button onclick={() => onOpen(n.id)}>{n.title ?? displayName(n.path)}<span class="path">{n.path}</span></button></li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .tags { display: flex; flex-direction: column; min-height: 0; overflow: auto; }
  .cloud { display: flex; flex-wrap: wrap; gap: 0.3rem; padding: 0.5rem; }
  .chip { font: inherit; font-size: 0.8rem; border: 1px solid var(--border); background: var(--bg); color: var(--accent); border-radius: 999px; padding: 0.1rem 0.6rem; cursor: pointer; }
  .chip span { color: var(--muted); margin-left: 0.3em; }
  .chip.on { background: var(--accent-bg); border-color: var(--accent); }
  ul { list-style: none; margin: 0; padding: 0 0.4rem; }
  li button { width: 100%; text-align: left; border: 0; background: none; color: inherit; font: inherit; font-size: 0.9rem; padding: 0.3rem 0.4rem; border-radius: 6px; cursor: pointer; display: flex; justify-content: space-between; gap: 0.5rem; }
  li button:hover { background: var(--hover); }
  .path { color: var(--muted); font-size: 0.8em; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .empty { color: var(--muted); padding: 0.5rem; font-size: 0.85rem; }
</style>
