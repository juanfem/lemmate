<script lang="ts">
  import { api, type NoteSummary } from '../lib/api.ts'
  import { displayName } from '../lib/vault.svelte.ts'

  let {
    vault,
    version,
    onOpen,
    selected = $bindable(null),
  }: {
    vault: string
    version: number
    onOpen: (id: string) => void
    /** Which tag is being listed. Owned by the shell, because a tag chip at the foot of a note
     *  picks one too, and the choice has to outlive this pane being swapped for another. */
    selected?: string | null
  } = $props()
  let tags: { tag: string; count: number }[] = $state([])
  let notes: NoteSummary[] = $state([])

  $effect(() => {
    version // reload whenever the vault changes
    api.tags(vault).then((t) => (tags = t)).catch(() => (tags = []))
  })
  $effect(() => {
    const tag = selected
    const v = vault
    if (!tag) {
      notes = []
      return
    }
    let live = true
    api
      .tagged(v, tag)
      .then((n) => live && (notes = n))
      .catch(() => live && (notes = []))
    return () => {
      live = false
    }
  })
  const pick = (tag: string) => (selected = selected === tag ? null : tag)
</script>

<div class="tags">
  <div class="cloud">
    {#each tags as t (t.tag)}
      <button class="chip" class:on={t.tag === selected} onclick={() => pick(t.tag)}>#{t.tag} <span>{t.count}</span></button>
    {/each}
    {#if tags.length === 0}<p class="empty">No tags yet. Write <code>#tag</code> in a note, or list <code>tags:</code> in its front matter.</p>{/if}
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

  /* Touch: a list row is a target, not a line of text. */
  @media (pointer: coarse) {
    li button {
      padding-top: 0.5rem;
      padding-bottom: 0.5rem;
    }
  }
</style>
