<script lang="ts">
  import type { NoteEntry } from '../lib/vault.svelte.ts'
  import { displayName } from '../lib/vault.svelte.ts'

  /** One vault as a root of the tree: its label, its notes, and whether it is the focused one. */
  export interface VaultNode {
    id: string
    label: string
    notes: NoteEntry[]
  }

  let {
    vaults,
    activeId,
    activeVault,
    onOpen,
    onCreateIn,
    onRenameFolder,
    onDeleteFolder,
    onCreateInVault,
    onRenameVault,
    onImportInto,
    onNewVault,
  }: {
    vaults: VaultNode[]
    activeId: string | null
    activeVault?: string | null
    onOpen: (id: string) => void
    onCreateIn?: (vault: string, folder: string) => void
    onRenameFolder?: (vault: string, folder: string) => void
    onDeleteFolder?: (vault: string, folder: string) => void
    onCreateInVault?: (vault: string) => void
    onRenameVault?: (vault: string) => void
    onImportInto?: (vault: string) => void
    onNewVault?: () => void
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

  // Folder collapse state is keyed by `<vault>/<folder path>`, so two vaults with a `Daily/`
  // folder collapse independently.
  let trees = $derived(vaults.map((v) => ({ vault: v, root: buildTree(v.notes) })))
  let collapsed: Record<string, boolean> = $state(loadCollapsed())

  function loadCollapsed(): Record<string, boolean> {
    try {
      return JSON.parse(localStorage.getItem('lemmate.tree.collapsed') ?? '{}')
    } catch {
      return {}
    }
  }
  function toggle(key: string) {
    collapsed[key] = !collapsed[key]
    try {
      localStorage.setItem('lemmate.tree.collapsed', JSON.stringify(collapsed))
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

{#snippet folder(vault: string, f: Folder, depth: number)}
  {#each [...f.folders.values()].sort((a, b) => a.name.localeCompare(b.name)) as sub (sub.path)}
    <div class="row folder" style:padding-left="{depth * 0.9 + 0.4}rem">
      <button class="folder-main" onclick={() => toggle(`${vault}/${sub.path}`)}>
        <span class="chev" class:open={!collapsed[`${vault}/${sub.path}`]}>▸</span>
        <span class="name">{sub.name}</span>
        <span class="count">{count(sub)}</span>
      </button>
      <span class="actions">
        {#if onCreateIn}<button title="New note here" onclick={() => onCreateIn(vault, sub.path)}>＋</button>{/if}
        {#if onRenameFolder}<button title="Rename / move folder" onclick={() => onRenameFolder(vault, sub.path)}>✎</button>{/if}
        {#if onDeleteFolder}<button title="Move folder to trash" onclick={() => onDeleteFolder(vault, sub.path)}>🗑</button>{/if}
      </span>
    </div>
    {#if !collapsed[`${vault}/${sub.path}`]}
      {@render folder(vault, sub, depth + 1)}
    {/if}
  {/each}
  {#each f.notes as n (n.id)}
    <button class="row note" class:active={n.id === activeId} style:padding-left="{depth * 0.9 + 1.3}rem" onclick={() => onOpen(n.id)} title={n.path}>
      <span class="name">{displayName(n.path)}</span>
    </button>
  {/each}
{/snippet}

<nav class="tree">
  {#each trees as t (t.vault.id)}
    <div class="row vault" class:current={t.vault.id === activeVault}>
      <button class="folder-main" onclick={() => toggle(t.vault.id)} title={t.vault.id}>
        <span class="chev" class:open={!collapsed[t.vault.id]}>▸</span>
        <span class="name">{t.vault.label}</span>
        <span class="count">{t.vault.notes.length}</span>
      </button>
      <span class="actions">
        {#if onCreateInVault}<button title="New note in this vault" onclick={() => onCreateInVault(t.vault.id)}>＋</button>{/if}
        {#if onRenameVault}<button title="Rename vault" onclick={() => onRenameVault(t.vault.id)}>✎</button>{/if}
        {#if onImportInto}<button title="Import an Obsidian vault here" onclick={() => onImportInto(t.vault.id)}>⇥</button>{/if}
      </span>
    </div>
    {#if !collapsed[t.vault.id]}
      {@render folder(t.vault.id, t.root, 1)}
      {#if t.vault.notes.length === 0}
        <p class="empty small">Empty vault.</p>
      {/if}
    {/if}
  {/each}
  {#if onNewVault}
    <button class="row add" onclick={onNewVault}>＋ New vault</button>
  {/if}
  {#if vaults.length === 0}
    <p class="empty">No vaults yet.</p>
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
  .folder .name,
  .vault .name {
    font-weight: 600;
  }
  .vault {
    padding-right: 0.2rem;
    border-top: 1px solid var(--border);
  }
  .vault:first-child {
    border-top: 0;
  }
  /* A vault's own name shows as typed; only the fallback ("vault a1b2c3") is generated. */
  .vault .name {
    font-size: 0.8rem;
    letter-spacing: 0.02em;
    color: var(--muted);
  }
  .vault.current .name {
    color: var(--fg);
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
  .folder:hover .actions,
  .vault:hover .actions {
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
  .add {
    color: var(--muted);
    font-size: 0.85rem;
    margin-top: 0.2rem;
  }
  .empty {
    color: var(--muted);
    padding: 1rem;
  }
  .empty.small {
    padding: 0.2rem 0.4rem 0.4rem 1.7rem;
    font-size: 0.8rem;
  }
</style>
