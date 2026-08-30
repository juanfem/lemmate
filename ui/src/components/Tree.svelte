<script lang="ts">
  import type { NoteEntry } from '../lib/vault.svelte.ts'
  import { displayName } from '../lib/vault.svelte.ts'

  let {
    notes,
    activeId,
    onOpen,
    onCreateIn,
    onRenameFolder,
    onDeleteFolder,
  }: {
    notes: NoteEntry[]
    activeId: string | null
    onOpen: (id: string) => void
    onCreateIn?: (folder: string) => void
    onRenameFolder?: (folder: string) => void
    onDeleteFolder?: (folder: string) => void
  } = $props()

  interface Folder {
    name: string
    path: string
    folders: Map<string, Folder>
    notes: NoteEntry[]
  }

  function buildTree(entries: NoteEntry[]): Folder {
    const root: Folder = { name: '', path: '', folders: new Map(), notes: [] }
    for (const n of entries) {
      const parts = n.path.split('/')
      let cur = root
      for (const part of parts.slice(0, -1)) {
        let next = cur.folders.get(part)
        if (!next) {
          next = { name: part, path: cur.path ? `${cur.path}/${part}` : part, folders: new Map(), notes: [] }
          cur.folders.set(part, next)
        }
        cur = next
      }
      cur.notes.push(n)
    }
    return root
  }

  let tree = $derived(buildTree(notes))
  let collapsed: Record<string, boolean> = $state(loadCollapsed())

  function loadCollapsed(): Record<string, boolean> {
    try {
      return JSON.parse(localStorage.getItem('notes.tree.collapsed') ?? '{}')
    } catch {
      return {}
    }
  }
  function toggle(path: string) {
    collapsed[path] = !collapsed[path]
    try {
      localStorage.setItem('notes.tree.collapsed', JSON.stringify(collapsed))
    } catch {
      /* ignore */
    }
  }
  function count(f: Folder): number {
    let n = f.notes.length
    for (const sub of f.folders.values()) n += count(sub)
    return n
  }
</script>

{#snippet folder(f: Folder, depth: number)}
  {#each [...f.folders.values()].sort((a, b) => a.name.localeCompare(b.name)) as sub (sub.path)}
    <div class="row folder" style:padding-left="{depth * 0.9 + 0.4}rem">
      <button class="folder-main" onclick={() => toggle(sub.path)}>
        <span class="chev" class:open={!collapsed[sub.path]}>▸</span>
        <span class="name">{sub.name}</span>
        <span class="count">{count(sub)}</span>
      </button>
      <span class="actions">
        {#if onCreateIn}<button title="New note here" onclick={() => onCreateIn(sub.path)}>＋</button>{/if}
        {#if onRenameFolder}<button title="Rename / move folder" onclick={() => onRenameFolder(sub.path)}>✎</button>{/if}
        {#if onDeleteFolder}<button title="Move folder to trash" onclick={() => onDeleteFolder(sub.path)}>🗑</button>{/if}
      </span>
    </div>
    {#if !collapsed[sub.path]}
      {@render folder(sub, depth + 1)}
    {/if}
  {/each}
  {#each f.notes as n (n.id)}
    <button class="row note" class:active={n.id === activeId} style:padding-left="{depth * 0.9 + 1.3}rem" onclick={() => onOpen(n.id)} title={n.path}>
      <span class="name">{displayName(n.path)}</span>
    </button>
  {/each}
{/snippet}

<nav class="tree">
  {@render folder(tree, 0)}
  {#if notes.length === 0}
    <p class="empty">No notes yet. Press <kbd>Ctrl</kbd>+<kbd>N</kbd> to create one.</p>
  {/if}
</nav>

<style>
  .tree {
    overflow: auto;
    font-size: 0.9rem;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    width: 100%;
    border: 0;
    background: none;
    color: inherit;
    text-align: left;
    padding: 0.2rem 0.4rem;
    cursor: pointer;
    border-radius: 4px;
    font: inherit;
  }
  .row:hover {
    background: var(--hover);
  }
  .row.active {
    background: var(--accent-bg);
    color: var(--accent);
  }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .folder .name {
    font-weight: 600;
  }
  .folder {
    padding-right: 0.2rem;
  }
  .folder-main {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 0.3rem;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    padding: 0;
    cursor: pointer;
    min-width: 0;
  }
  .actions {
    display: none;
    gap: 0.1rem;
  }
  .folder:hover .actions {
    display: inline-flex;
  }
  .actions button {
    border: 0;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: 0.8em;
    cursor: pointer;
    padding: 0 0.2rem;
  }
  .actions button:hover {
    color: var(--fg);
  }
  .chev {
    display: inline-block;
    width: 0.8em;
    transition: transform 0.1s;
    color: var(--muted);
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .count {
    color: var(--muted);
    font-size: 0.75em;
  }
  .empty {
    color: var(--muted);
    padding: 1rem;
  }
</style>
