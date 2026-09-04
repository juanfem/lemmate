<script lang="ts" module>
  export interface Command {
    id: string
    label: string
    shortcut?: string
    run: () => void
  }

  /** How well `path` matches `q`, or -1 for no match. Substring beats subsequence, an early
   *  hit beats a late one, and a short path beats a long one at the same position. */
  export function score(path: string, q: string): number {
    const p = path.toLowerCase()
    if (p.includes(q)) return 100 - p.indexOf(q) - (p.length - q.length) / 100
    let i = 0
    for (const ch of p) if (ch === q[i]) i++
    return i === q.length ? 50 - p.length / 100 : -1
  }
</script>

<script lang="ts">
  import { untrack } from 'svelte'
  import { api, type SearchHit } from '../lib/api.ts'
  import { displayName } from '../lib/vault.svelte.ts'
  import type { WorkspaceNote } from '../lib/workspace.svelte.ts'

  /**
   * One palette over everything: note titles, folders, full text and commands (SPEC §9).
   *
   * There used to be two overlays — a quick switcher for notes and a command palette for
   * actions — which meant knowing which one held the thing you wanted before you could ask
   * for it. They are the same gesture, so they are now the same surface, and full-text hits
   * join them rather than living in a sidebar pane of their own.
   *
   * A leading `>` narrows to commands, the way an editor's palette does; that is what
   * Ctrl+Shift+P now opens with, so the old "show me every command" habit still works.
   */
  type Row =
    | { kind: 'Note'; key: string; name: string; path: string; id: string }
    | { kind: 'Folder'; key: string; name: string; path: string; vault: string; folder: string }
    | { kind: 'Text'; key: string; name: string; path: string; id: string }
    | { kind: 'Action'; key: string; name: string; path: string; run: () => void }
    | { kind: 'New'; key: string; name: string; path: string; create: string }

  let {
    notes,
    folders,
    commands,
    label,
    createVault,
    initial = '',
    onOpen,
    onOpenInPane,
    onCreate,
    onFolder,
    onClose,
  }: {
    notes: WorkspaceNote[]
    folders: { vault: string; folder: string }[]
    commands: Command[]
    /** Vault label beside a hit; empty when a single vault makes it noise. */
    label?: (vault: string) => string
    createVault?: string | null
    /** Prefilled query — `>` from Ctrl+Shift+P, empty from Ctrl+K. */
    initial?: string
    onOpen: (id: string) => void
    onOpenInPane: (id: string) => void
    onCreate: (path: string) => void
    onFolder: (vault: string, folder: string) => void
    onClose: () => void
  } = $props()

  // A seed, not a binding: `initial` says what the palette opens with, and typing must not be
  // fighting a prop. Re-opening with a different seed remounts the component (see App).
  let query = $state(untrack(() => initial))
  let selected = $state(0)
  let input: HTMLInputElement
  let list: HTMLUListElement | undefined = $state()

  let commandsOnly = $derived(query.startsWith('>'))
  let term = $derived((commandsOnly ? query.slice(1) : query).trim())
  let q = $derived(term.toLowerCase())

  // ---- full text, from the server's index rather than from anything held here
  //
  // Debounced and generation-guarded: typing fires one request per pause, and a slow reply for
  // "rec" must not land on top of the results for "recetas".
  let hits: SearchHit[] = $state([])
  let generation = 0
  $effect(() => {
    const text = q
    if (commandsOnly || text.length < 2) {
      hits = []
      return
    }
    const mine = ++generation
    const timer = setTimeout(() => {
      api
        .searchAll(text, 8)
        .then((h) => mine === generation && (hits = h))
        .catch(() => mine === generation && (hits = []))
    }, 150)
    return () => clearTimeout(timer)
  })

  function normalize(text: string): string {
    const t = text.trim().replace(/^\/+/u, '')
    return t.endsWith('.md') || t.endsWith('.qmd') ? t : `${t}.md`
  }

  let rows: Row[] = $derived.by(() => {
    const actions: Row[] = commands
      .filter((c) => !q || c.label.toLowerCase().includes(q))
      .map((c) => ({ kind: 'Action' as const, key: `a:${c.id}`, name: c.label, path: c.shortcut ?? '', run: c.run }))
    if (commandsOnly) return actions.slice(0, 40)

    if (!q) {
      return [
        ...notes.slice(0, 30).map((n) => ({ kind: 'Note' as const, key: `n:${n.id}`, name: displayName(n.path), path: n.path, id: n.id })),
        ...actions.slice(0, 10),
      ]
    }

    // Notes, folders and actions are ranked together so the best answer is first whatever kind
    // it happens to be; full-text hits go last because matching a title beats matching a body.
    const scored: { row: Row; s: number }[] = []
    for (const n of notes) {
      const s = score(n.path, q)
      if (s >= 0) scored.push({ row: { kind: 'Note', key: `n:${n.id}`, name: displayName(n.path), path: n.path, id: n.id }, s })
    }
    for (const f of folders) {
      const s = score(f.folder, q)
      if (s >= 0)
        scored.push({
          row: { kind: 'Folder', key: `f:${f.vault}:${f.folder}`, name: f.folder.slice(f.folder.lastIndexOf('/') + 1), path: f.folder, vault: f.vault, folder: f.folder },
          s: s - 1,
        })
    }
    for (const a of actions) scored.push({ row: a, s: score(a.name, q) - 2 })
    scored.sort((x, y) => y.s - x.s)

    const seen = new Set(scored.map((x) => (x.row.kind === 'Note' ? x.row.id : '')))
    const text: Row[] = hits
      .filter((h) => !seen.has(h.note_id))
      .map((h) => {
        const n = notes.find((x) => x.id === h.note_id)
        return { kind: 'Text' as const, key: `t:${h.note_id}`, name: h.snippet.replace(/[[\]]/gu, '').trim(), path: n ? displayName(n.path) : '', id: h.note_id }
      })

    const out = [...scored.slice(0, 30).map((x) => x.row), ...text]
    if (createVault && !notes.some((n) => n.path === normalize(term)))
      out.push({ kind: 'New', key: 'new', name: `Create “${normalize(term)}”`, path: label?.(createVault) ?? '', create: normalize(term) })
    return out
  })

  function choose(i: number, e?: { metaKey: boolean; ctrlKey: boolean; shiftKey: boolean }) {
    const row = rows[i]
    if (!row) return
    // ⇧↵ makes a note out of what you typed, whatever the highlighted row happens to be.
    if (e?.shiftKey && createVault && term) {
      onClose()
      onCreate(normalize(term))
      return
    }
    onClose()
    if (row.kind === 'Action') row.run()
    else if (row.kind === 'Folder') onFolder(row.vault, row.folder)
    else if (row.kind === 'New') onCreate(row.create)
    else if (e?.metaKey || e?.ctrlKey) onOpenInPane(row.id)
    else onOpen(row.id)
  }

  function onKey(e: KeyboardEvent) {
    const total = Math.max(rows.length, 1)
    if (e.key === 'ArrowDown') {
      selected = (selected + 1) % total
      e.preventDefault()
    } else if (e.key === 'ArrowUp') {
      selected = (selected - 1 + total) % total
      e.preventDefault()
    } else if (e.key === 'Enter') {
      choose(selected, e)
      e.preventDefault()
    } else if (e.key === 'Escape') {
      onClose()
    }
  }

  $effect(() => {
    input?.focus()
  })
  $effect(() => {
    query
    selected = 0
  })
  // Keep the highlighted row in view when the arrows walk past the edge of the list.
  $effect(() => {
    selected
    list?.querySelector('li.selected')?.scrollIntoView({ block: 'nearest' })
  })
</script>

<div class="backdrop" onmousedown={onClose} role="presentation">
  <div class="dialog" onmousedown={(e) => e.stopPropagation()} role="dialog" aria-label="Search and commands" tabindex="-1">
    <div class="field">
      <span class="icon" aria-hidden="true">⌕</span>
      <input bind:this={input} bind:value={query} onkeydown={onKey} placeholder="Search notes, folders, text and commands…" aria-label="Search notes, folders, text and commands" />
      <kbd>esc</kbd>
    </div>
    <ul bind:this={list}>
      {#each rows as r, i (r.key)}
        <li class:selected={i === selected}>
          <button onclick={(e) => choose(i, e)}>
            <span class="kind">{r.kind === 'New' ? 'New' : r.kind}</span>
            <span class="name">{r.name}</span>
            <span class="where">{r.path}</span>
          </button>
        </li>
      {/each}
      {#if rows.length === 0}<li class="none">Nothing matches.</li>{/if}
    </ul>
    <div class="hints">
      <span><kbd>↑↓</kbd> navigate</span>
      <span><kbd>↵</kbd> open</span>
      <span><kbd>⌘↵</kbd> open in split</span>
      <span><kbd>⇧↵</kbd> new note</span>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgb(28 24 20 / 0.28);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding-top: 12vh;
    z-index: 10;
    overflow: auto;
  }
  .dialog {
    width: min(35rem, 92vw);
    background: var(--bg);
    border-radius: 12px;
    box-shadow:
      0 24px 60px -12px rgb(20 16 12 / 0.45),
      0 0 0 0.5px rgb(0 0 0 / 0.16);
    overflow: hidden;
    animation: palette-in 140ms ease-out;
  }
  @keyframes palette-in {
    from {
      opacity: 0;
      transform: translateY(-8px) scale(0.985);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .dialog {
      animation: none;
    }
  }
  .field {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.875rem 1.125rem;
    border-bottom: 1px solid var(--border-soft);
  }
  .field .icon {
    color: var(--faint);
  }
  input {
    flex: 1;
    min-width: 0;
    font: inherit;
    font-size: 1rem;
    border: 0;
    background: transparent;
    color: inherit;
    outline: none;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0.375rem;
    max-height: 50vh;
    overflow: auto;
  }
  li button {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    padding: 0.5rem 0.75rem;
    border-radius: 7px;
    text-align: left;
    cursor: pointer;
  }
  li.selected button,
  li button:hover {
    background: var(--accent-bg);
  }
  /* A fixed column, so the eye can run down the kinds without reading them. */
  .kind {
    flex: none;
    width: 3.25rem;
    font-size: 0.6875rem;
    color: var(--faint);
  }
  .name {
    min-width: 0;
    font-size: 0.85rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .where {
    margin-left: auto;
    flex: none;
    max-width: 40%;
    font-size: 0.6875rem;
    color: var(--faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .none {
    padding: 0.75rem;
    font-size: 0.8rem;
    color: var(--faint);
  }
  .hints {
    display: flex;
    gap: 1rem;
    padding: 0.5rem 1.125rem;
    border-top: 1px solid var(--border-soft);
    background: var(--chrome);
    font-size: 0.6875rem;
    color: var(--muted);
  }
  .hints kbd {
    border: 0;
    padding: 0;
    font-family: inherit;
  }

  /* A phone has no room to spare above an overlay, and the hint row is four shortcuts it has
     no keys for. */
  @media (max-width: 720px) {
    .backdrop {
      padding-top: 5vh;
    }
    .hints {
      display: none;
    }
  }
  @media (pointer: coarse) {
    li button {
      padding-top: 0.6rem;
      padding-bottom: 0.6rem;
    }
  }
</style>
