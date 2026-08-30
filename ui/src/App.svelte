<script lang="ts">
  import { onDestroy, untrack } from 'svelte'
  import { api, type VaultInfo, type NoteSummary } from './lib/api.ts'
  import { VaultSession, displayName } from './lib/vault.svelte.ts'
  import { ulid } from './lib/ulid.ts'
  import Tree from './components/Tree.svelte'
  import Editor from './components/Editor.svelte'
  import QuickSwitcher from './components/QuickSwitcher.svelte'
  import SearchPane from './components/SearchPane.svelte'
  import TagsPane from './components/TagsPane.svelte'
  import OutlinePane, { type OutlineItem } from './components/OutlinePane.svelte'
  import CommandPalette, { type Command } from './components/CommandPalette.svelte'

  // ---- vault selection: #/v/<ULID>
  let vaultId = $state<string | null>(readHash())
  let vaults: VaultInfo[] = $state([])
  let session = $state<VaultSession | null>(null)

  function readHash(): string | null {
    const m = /^#\/v\/([0-9A-HJKMNP-TV-Z]{26})/u.exec(location.hash)
    return m ? m[1]! : null
  }
  window.addEventListener('hashchange', () => (vaultId = readHash()))

  $effect(() => {
    const id = vaultId
    // Only `vaultId` is a dependency: everything else here is written, not tracked.
    untrack(() => {
      session?.destroy()
      session = id ? new VaultSession(id) : null
      // Debug/automation handle (used by scripts/cdp.mjs smoke runs).
      ;(window as unknown as { notes?: unknown }).notes = { session }
      tabs = []
      active = null
    })
    if (!vaultId)
      api
        .vaults()
        .then((v) => {
          vaults = v
          // A local relay serves exactly one vault: go straight in.
          if (v.length === 1) openVault(v[0]!.id)
        })
        .catch(() => (vaults = []))
  })
  onDestroy(() => session?.destroy())

  function openVault(id: string) {
    location.hash = `#/v/${id}`
  }
  function newVault() {
    openVault(ulid())
  }

  // ---- tabs
  let tabs: string[] = $state([])
  let active = $state<string | null>(null)
  let sidebar: 'files' | 'search' | 'tags' | 'outline' | 'bookmarks' = $state('files')
  let switcher = $state(false)
  let palette = $state(false)
  let backlinks: NoteSummary[] = $state([])
  let headings: OutlineItem[] = $state([])
  let jumpTo: ((pos: number) => void) | undefined = $state()
  let tagsVersion = $state(0)

  function open(id: string) {
    if (!tabs.includes(id)) tabs = [...tabs, id]
    active = id
    switcher = false
    palette = false
    headings = []
    refreshBacklinks()
    tagsVersion++
  }
  function openPath(path: string) {
    const id = session?.idOf(path)
    if (id) open(id)
  }
  function bookmarkActive() {
    if (!session || !active) return
    const path = session.pathOf(active)
    if (path) session.toggleBookmark({ kind: 'note', target: path, label: displayName(path) })
  }
  let commands: Command[] = $derived([
    { id: 'open', label: 'Open or create note…', shortcut: 'Ctrl+O', run: () => (switcher = true) },
    { id: 'daily', label: "Open today's daily note", shortcut: 'Ctrl+Shift+D', run: daily },
    { id: 'search', label: 'Search notes', shortcut: 'Ctrl+Shift+F', run: () => (sidebar = 'search') },
    { id: 'files', label: 'Show files', run: () => (sidebar = 'files') },
    { id: 'tags', label: 'Show tags', run: () => (sidebar = 'tags') },
    { id: 'outline', label: 'Show outline', run: () => (sidebar = 'outline') },
    { id: 'bookmarks', label: 'Show bookmarks', run: () => (sidebar = 'bookmarks') },
    { id: 'bookmark', label: session && active && session.isBookmarked('note', session.pathOf(active) ?? '') ? 'Remove bookmark' : 'Bookmark this note', shortcut: 'Ctrl+Shift+B', run: bookmarkActive },
    { id: 'rename', label: 'Rename / move note', run: renameActive },
    { id: 'delete', label: 'Move note to trash', run: deleteActive },
    { id: 'close', label: 'Close tab', shortcut: 'Ctrl+W', run: () => active && close(active) },
    { id: 'vault', label: 'Switch vault', run: () => (location.hash = '') },
  ])
  function close(id: string) {
    const i = tabs.indexOf(id)
    tabs = tabs.filter((t) => t !== id)
    if (active === id) active = tabs[Math.min(i, tabs.length - 1)] ?? null
  }
  /** Templates (SPEC §9): `Templates/<name>.md` with {{date}}, {{date:FORMAT}}, {{time}}, {{title}}. */
  async function template(name: string, title: string, fallback: string): Promise<string> {
    if (!session) return fallback
    const id = session.idOf(`Templates/${name}.md`)
    if (!id) return fallback
    const { doc, release } = session.acquire(id)
    try {
      await new Promise<void>((resolve) => {
        if (doc.getText('content').length > 0 || session!.client.isSynced(id)) return resolve()
        const t = setTimeout(resolve, 3000)
        doc.getText('content').observe(() => (clearTimeout(t), resolve()))
      })
      const raw = doc.getText('content').toString()
      const body = raw.startsWith('---\n') ? raw.slice(raw.indexOf('\n---', 4) + 4).replace(/^\n/u, '') : raw
      return fillTemplate(body, title)
    } finally {
      release()
    }
  }
  function fillTemplate(body: string, title: string): string {
    const d = new Date()
    const pad = (n: number) => String(n).padStart(2, '0')
    const fmt = (f: string) =>
      f
        .replace(/YYYY/gu, String(d.getFullYear()))
        .replace(/MM/gu, pad(d.getMonth() + 1))
        .replace(/DD/gu, pad(d.getDate()))
        .replace(/HH/gu, pad(d.getHours()))
        .replace(/mm/gu, pad(d.getMinutes()))
    return body
      .replace(/\{\{date:([^}]+)\}\}/gu, (_, f: string) => fmt(f))
      .replace(/\{\{date\}\}/gu, fmt('YYYY-MM-DD'))
      .replace(/\{\{time\}\}/gu, fmt('HH:mm'))
      .replace(/\{\{title\}\}/gu, title)
      .replace(/\{\{cursor\}\}/gu, '')
  }
  async function create(path: string) {
    if (!session) return
    const title = displayName(path)
    open(session.createNote(path, await template('Note', title, `# ${title}\n\n`)))
  }
  async function daily() {
    if (!session) return
    const d = new Date()
    const name = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
    const path = `Daily/${name}.md`
    const existing = session.idOf(path)
    open(existing ?? session.createNote(path, await template('Daily', name, `# ${name}\n\n`)))
  }
  function renameActive() {
    if (!session || !active) return
    const current = session.pathOf(active) ?? ''
    const next = prompt('New path', current)?.trim()
    if (next && next !== current) session.renameNote(active, next.endsWith('.md') || next.endsWith('.qmd') ? next : `${next}.md`)
  }
  function deleteActive() {
    if (!session || !active) return
    const path = session.pathOf(active) ?? active
    if (confirm(`Move “${path}” to trash?`)) {
      session.deleteNote(active)
      close(active)
    }
  }
  async function refreshBacklinks() {
    if (!session || !active) return (backlinks = [])
    try {
      backlinks = await api.backlinks(session.id, active)
    } catch {
      backlinks = []
    }
  }

  function onKey(e: KeyboardEvent) {
    const mod = e.ctrlKey || e.metaKey
    if (!mod) return
    if ((e.key === 'o' || e.key === 'p') && !e.shiftKey) {
      switcher = !switcher
      palette = false
      e.preventDefault()
    } else if (e.key === 'n' && !e.shiftKey) {
      switcher = true
      e.preventDefault()
    } else if (e.key === 'w') {
      if (active) close(active)
      e.preventDefault()
    } else if (e.key === 'd' && e.shiftKey) {
      daily()
      e.preventDefault()
    } else if (e.key === 'f' && e.shiftKey) {
      sidebar = 'search'
      e.preventDefault()
    } else if ((e.key === 'p' || e.key === 'P') && e.shiftKey) {
      palette = !palette
      switcher = false
      e.preventDefault()
    } else if ((e.key === 'b' || e.key === 'B') && e.shiftKey) {
      bookmarkActive()
      e.preventDefault()
    }
  }

  let activePath = $derived(session && active ? (session.pathOf(active) ?? '(deleted)') : '')
</script>

<svelte:window onkeydown={onKey} />

{#if !session}
  <main class="welcome">
    <h1>notes</h1>
    <p>Open a vault on this server, or create a new one.</p>
    <ul>
      {#each vaults as v (v.id)}
        <li><button onclick={() => openVault(v.id)}>{v.id} <span class="muted">({v.notes} notes)</span></button></li>
      {/each}
    </ul>
    <button class="primary" onclick={newVault}>New vault</button>
  </main>
{:else}
  <div class="layout">
    <aside>
      <div class="side-tabs">
        <button class:on={sidebar === 'files'} onclick={() => (sidebar = 'files')} title="Files">Files</button>
        <button class:on={sidebar === 'search'} onclick={() => (sidebar = 'search')} title="Search (Ctrl+Shift+F)">Search</button>
        <button class:on={sidebar === 'tags'} onclick={() => (sidebar = 'tags')} title="Tags">Tags</button>
        <button class:on={sidebar === 'outline'} onclick={() => (sidebar = 'outline')} title="Outline">Outline</button>
        <button class:on={sidebar === 'bookmarks'} onclick={() => (sidebar = 'bookmarks')} title="Bookmarks">★</button>
        <span class="spacer"></span>
        <button title="New note (Ctrl+N)" onclick={() => (switcher = true)}>＋</button>
        <button title="Command palette (Ctrl+Shift+P)" onclick={() => (palette = true)}>⌘</button>
      </div>
      {#if sidebar === 'files'}
        <Tree notes={session.notes} activeId={active} onOpen={open} />
      {:else if sidebar === 'search'}
        <SearchPane vault={session.id} onOpen={open} />
      {:else if sidebar === 'tags'}
        <TagsPane vault={session.id} version={tagsVersion} onOpen={open} />
      {:else if sidebar === 'outline'}
        <OutlinePane items={headings} onJump={(pos) => jumpTo?.(pos)} />
      {:else}
        <nav class="bookmarks-pane">
          {#each session.bookmarks as b, i (b.kind + b.target + i)}
            <button onclick={() => openPath(b.target)} title={b.target}>★ {b.label}</button>
          {/each}
          {#if session.bookmarks.length === 0}<p class="muted pad">Bookmark a note with <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>B</kbd>.</p>{/if}
        </nav>
      {/if}
      <footer class="status" class:offline={session.status !== 'online'}>
        <span class="dot"></span>
        {session.status}{#if session.status === 'online' && !session.vaultSynced} · syncing…{/if}
        · {session.notes.length} notes
      </footer>
    </aside>
    <section class="main">
      <div class="tabs">
        {#each tabs as id (id)}
          <button class="tab" class:active={id === active} onclick={() => open(id)} title={session.pathOf(id)}>
            {displayName(session.pathOf(id) ?? id)}
            <span class="x" role="button" tabindex="-1" onclick={(e) => { e.stopPropagation(); close(id) }} onkeydown={() => {}}>×</span>
          </button>
        {/each}
      </div>
      {#if active}
        {#key active}
          <div class="note-head">
            <span class="path">{activePath}</span>
            <span class="spacer"></span>
            <button onclick={bookmarkActive} title="Bookmark (Ctrl+Shift+B)">{session.isBookmarked('note', activePath) ? '★' : '☆'}</button>
            <button onclick={renameActive} title="Rename / move">Rename</button>
            <button onclick={deleteActive} title="Move to trash">Delete</button>
          </div>
          <div class="editor-wrap">
            <Editor {session} noteId={active} onOpen={open} onHeadings={(h) => (headings = h)} bind:jumpTo />
          </div>
          {#if backlinks.length}
            <div class="backlinks">
              <strong>Linked from</strong>
              {#each backlinks as b (b.id)}
                <button onclick={() => open(b.id)}>{b.title ?? displayName(b.path)}</button>
              {/each}
            </div>
          {/if}
        {/key}
      {:else}
        <div class="placeholder">
          <p>Open a note from the tree, or press <kbd>Ctrl</kbd>+<kbd>O</kbd>.</p>
        </div>
      {/if}
    </section>
  </div>
  {#if switcher}
    <QuickSwitcher notes={session.notes} onOpen={open} onCreate={create} onClose={() => (switcher = false)} />
  {/if}
  {#if palette}
    <CommandPalette {commands} onClose={() => (palette = false)} />
  {/if}
{/if}

<style>
  .welcome {
    max-width: 30rem;
    margin: 10vh auto;
    padding: 1rem;
  }
  .welcome ul {
    list-style: none;
    padding: 0;
  }
  .welcome li button {
    font: inherit;
    font-family: var(--mono);
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.5rem 0.8rem;
    margin: 0.2rem 0;
    cursor: pointer;
    color: inherit;
    width: 100%;
    text-align: left;
  }
  .primary {
    font: inherit;
    background: var(--accent);
    color: white;
    border: 0;
    border-radius: 6px;
    padding: 0.5rem 1rem;
    cursor: pointer;
  }
  .muted {
    color: var(--muted);
  }
  .layout {
    display: grid;
    grid-template-columns: 17rem 1fr;
    height: 100%;
  }
  aside {
    background: var(--panel);
    border-right: 1px solid var(--border);
    display: grid;
    grid-template-rows: auto 1fr auto;
    min-height: 0;
  }
  .side-tabs {
    display: flex;
    gap: 0.2rem;
    padding: 0.4rem;
    border-bottom: 1px solid var(--border);
  }
  .side-tabs button,
  .note-head button,
  .backlinks button {
    font: inherit;
    font-size: 0.85rem;
    border: 0;
    background: none;
    color: var(--muted);
    padding: 0.2rem 0.5rem;
    border-radius: 4px;
    cursor: pointer;
  }
  .side-tabs button.on {
    color: var(--fg);
    background: var(--hover);
  }
  .spacer {
    flex: 1;
  }
  .status {
    font-size: 0.75rem;
    color: var(--muted);
    padding: 0.3rem 0.6rem;
    border-top: 1px solid var(--border);
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .dot {
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: #22c55e;
  }
  .offline .dot {
    background: #f59e0b;
  }
  .main {
    display: grid;
    grid-template-rows: auto auto 1fr auto;
    min-height: 0;
  }
  .tabs {
    display: flex;
    overflow-x: auto;
    border-bottom: 1px solid var(--border);
    background: var(--panel);
  }
  .tab {
    font: inherit;
    font-size: 0.85rem;
    border: 0;
    border-right: 1px solid var(--border);
    background: none;
    color: var(--muted);
    padding: 0.4rem 0.8rem;
    cursor: pointer;
    white-space: nowrap;
  }
  .tab.active {
    color: var(--fg);
    background: var(--bg);
  }
  .tab .x {
    margin-left: 0.5rem;
    opacity: 0.6;
  }
  .note-head {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.2rem 1rem;
    font-size: 0.8rem;
    color: var(--muted);
    border-bottom: 1px solid var(--border);
  }
  .editor-wrap {
    min-height: 0;
  }
  .backlinks {
    border-top: 1px solid var(--border);
    padding: 0.4rem 1rem;
    font-size: 0.85rem;
    display: flex;
    gap: 0.4rem;
    flex-wrap: wrap;
    align-items: center;
  }
  .backlinks button {
    color: var(--accent);
  }
  .bookmarks-pane {
    display: flex;
    flex-direction: column;
    padding: 0.3rem;
    overflow: auto;
  }
  .bookmarks-pane button {
    font: inherit;
    font-size: 0.9rem;
    text-align: left;
    border: 0;
    background: none;
    color: inherit;
    padding: 0.25rem 0.5rem;
    border-radius: 4px;
    cursor: pointer;
  }
  .bookmarks-pane button:hover {
    background: var(--hover);
  }
  .pad {
    padding: 0.6rem;
    font-size: 0.85rem;
  }
  .placeholder {
    display: grid;
    place-items: center;
    color: var(--muted);
  }
</style>
