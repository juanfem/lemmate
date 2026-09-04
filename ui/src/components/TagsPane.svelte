<script lang="ts">
  import { untrack } from 'svelte'
  import { api, type NoteSummary } from '../lib/api.ts'
  import { displayName } from '../lib/vault.svelte.ts'
  import { buildTagTree, tagAncestors, type TagNode } from '../lib/tags.ts'
  import { longpress } from '../lib/longpress.ts'
  import Icon from './Icon.svelte'

  let {
    vault,
    version,
    onOpen,
    onMenu,
    selected = $bindable(null),
  }: {
    vault: string
    version: number
    onOpen: (id: string) => void
    /** Right-click, or a long press: renaming and deleting are the shell's to offer. */
    onMenu?: (tag: string, e: MouseEvent) => void
    /** Which tag is being listed. Owned by the shell, because a tag chip at the foot of a note
     *  picks one too, and the choice has to outlive this pane being swapped for another. */
    selected?: string | null
  } = $props()
  let tags: { tag: string; count: number }[] = $state([])
  let notes: NoteSummary[] = $state([])
  /**
   * A tree, because that is what tags already are: asking for `#projects` has always answered
   * with `#projects/alpha` as well, and a listing that draws them as unrelated full strings is
   * the one place in the app that pretends otherwise.
   */
  let tree = $derived(buildTagTree(tags))

  // Which branches are shut, per vault, remembered the way the file tree remembers its folders.
  // Absent means open: a tree that hides everything it knows on first sight is not a view.
  let collapsed: Record<string, boolean> = $state(stored('lemmate.tags.collapsed', {}))
  const key = (tag: string) => `${vault}/${tag}`

  function stored<T>(k: string, fallback: T): T {
    try {
      const raw = localStorage.getItem(k)
      return raw === null ? fallback : (JSON.parse(raw) as T)
    } catch {
      return fallback
    }
  }
  function save() {
    try {
      localStorage.setItem('lemmate.tags.collapsed', JSON.stringify(collapsed))
    } catch {
      /* private mode, quota — the view just forgets */
    }
  }
  function toggle(tag: string) {
    collapsed[key(tag)] = !collapsed[key(tag)]
    save()
  }

  /** Every tag with something under it — the rows the two buttons in the toolbar are about. */
  function branches(nodes: TagNode[], out: string[] = []): string[] {
    for (const n of nodes)
      if (n.children.length) {
        out.push(n.tag)
        branches(n.children, out)
      }
    return out
  }
  let foldable = $derived(branches(tree))
  // Only this vault's keys move: one record holds them all, and a vault's folds are its own.
  function expandAll() {
    collapsed = Object.fromEntries(Object.entries(collapsed).filter(([k]) => !k.startsWith(`${vault}/`)))
    save()
  }
  function collapseAll() {
    collapsed = { ...collapsed, ...Object.fromEntries(foldable.map((t) => [key(t), true])) }
    save()
  }

  $effect(() => {
    version // reload whenever the vault, or anything that moves a tag, changes
    const v = vault
    let live = true
    const load = () =>
      api
        .tags(v)
        .then((t) => live && (tags = t))
        .catch(() => live && (tags = []))
    void load()
    // Asked twice, because this listing is derived on the *far* side of the socket. Whatever
    // moved a tag — a keystroke here, a rename across the vault, another device — reaches the
    // index a beat after it reaches us, so an answer fetched the instant the edit lands is the
    // tags as they were. The second ask is that beat.
    const settled = setTimeout(load, 700)
    return () => {
      live = false
      clearTimeout(settled)
    }
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
  // A nested tag picked from somewhere else — a chip at the foot of a note — has to be visible
  // once it is selected, so the branches above it open.
  $effect(() => {
    const tag = selected
    const v = vault
    if (!tag) return
    untrack(() => {
      let opened = false
      for (const a of tagAncestors(tag)) {
        if (collapsed[`${v}/${a}`]) {
          delete collapsed[`${v}/${a}`]
          opened = true
        }
      }
      if (opened) {
        collapsed = { ...collapsed }
        save()
      }
    })
  })
  const pick = (tag: string) => (selected = selected === tag ? null : tag)
</script>

{#snippet rows(nodes: TagNode[], depth: number)}
  {#each nodes as n (n.tag)}
    <div class="row" class:selected={n.tag === selected} style:padding-left="{depth * 0.9 + 0.4}rem">
      {#if n.children.length}
        <button
          class="chev"
          class:open={!collapsed[key(n.tag)]}
          aria-label={collapsed[key(n.tag)] ? `Expand ${n.name}` : `Collapse ${n.name}`}
          onclick={() => toggle(n.tag)}>▸</button
        >
      {:else}
        <span class="chev spacer"></span>
      {/if}
      <button
        class="main"
        onclick={() => pick(n.tag)}
        oncontextmenu={(e) => (onMenu ? (e.preventDefault(), onMenu(n.tag, e)) : undefined)}
        use:longpress
        title={`#${n.tag}`}
      >
        <!-- The `#` marks where a tag starts; below the root the indentation supplies the rest,
             and repeating `projects/` down the branch is the thing this view is here to stop. -->
        <span class="name">{depth === 0 ? `#${n.name}` : n.name}</span>
        {#if n.count !== undefined}<span class="count">{n.count}</span>{/if}
      </button>
    </div>
    {#if n.children.length && !collapsed[key(n.tag)]}
      {@render rows(n.children, depth + 1)}
    {/if}
  {/each}
{/snippet}

<div class="tags">
  <!-- Only where there is something to fold: a vault whose tags are all flat has no tree to
       open or shut, and two buttons that do nothing are two buttons to wonder about. -->
  {#if foldable.length}
    <div class="toolbar">
      <span class="gap"></span>
      <button onclick={expandAll} title="Expand all" aria-label="Expand all"><Icon name="expand" /></button>
      <button onclick={collapseAll} title="Collapse all" aria-label="Collapse all"><Icon name="collapse" /></button>
    </div>
  {/if}
  <nav class="tree">
    {@render rows(tree, 0)}
    {#if tags.length === 0}<p class="empty">No tags yet. Write <code>#tag</code> in a note, or list <code>tags:</code> in its front matter.</p>{/if}
  </nav>
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
  /* The same strip as the file browser's, so the two tabs have the same furniture in the same
     place. */
  .toolbar { display: flex; align-items: center; gap: 0.1rem; padding: 0.25rem 0.4rem; border-bottom: 1px solid var(--border); }
  .toolbar .gap { flex: 1; }
  .toolbar button { display: grid; place-items: center; border: 0; background: none; color: var(--muted); padding: 0.25rem; border-radius: 4px; cursor: pointer; }
  .toolbar button:hover { background: var(--hover); color: var(--fg); }
  /* The same row, chevron and selection language as the folder tree next door: switching
     between the two tabs should not move the eye. */
  .tree { padding: 0.35rem 0.25rem; font-size: 0.9rem; }
  .row {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    width: 100%;
    padding: 0.2rem 0.4rem;
    border-radius: 4px;
  }
  .row:hover { background: var(--hover); }
  .row.selected { background: var(--accent); color: var(--accent-fg); }
  .row.selected .count, .row.selected .chev { color: inherit; opacity: 0.75; }
  .chev {
    flex: none;
    width: 1em;
    border: 0;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--muted);
    cursor: pointer;
    transition: transform 0.1s;
  }
  .chev.open { transform: rotate(90deg); }
  .chev.spacer { cursor: default; }
  .main {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    min-width: 0;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    padding: 0;
    cursor: pointer;
  }
  .name { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .count { flex: none; color: var(--muted); font-size: 0.8em; font-variant-numeric: tabular-nums; }
  ul { list-style: none; margin: 0; padding: 0 0.4rem 0.4rem; border-top: 1px solid var(--border-soft); }
  li button { width: 100%; text-align: left; border: 0; background: none; color: inherit; font: inherit; font-size: 0.9rem; padding: 0.3rem 0.4rem; border-radius: 6px; cursor: pointer; display: flex; justify-content: space-between; gap: 0.5rem; }
  li button:hover { background: var(--hover); }
  .path { color: var(--muted); font-size: 0.8em; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .empty { color: var(--muted); padding: 0.5rem; font-size: 0.85rem; }

  /* Touch: a row is a target, not a line of text. */
  @media (pointer: coarse) {
    .toolbar { gap: 0.25rem; padding: 0.4rem; }
    .toolbar button { padding: 0.45rem; }
    .row { padding-top: 0.4rem; padding-bottom: 0.4rem; }
    li button { padding-top: 0.5rem; padding-bottom: 0.5rem; }
  }
</style>
