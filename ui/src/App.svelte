<script lang="ts">
  import { onDestroy } from 'svelte'
  import { api, type VaultInfo, type NoteSummary } from './lib/api.ts'
  import { VaultSession, displayName } from './lib/vault.svelte.ts'
  import { ulid } from './lib/ulid.ts'
  import Tree from './components/Tree.svelte'
  import Editor from './components/Editor.svelte'
  import QuickSwitcher from './components/QuickSwitcher.svelte'
  import SearchPane from './components/SearchPane.svelte'

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
    session?.destroy()
    session = vaultId ? new VaultSession(vaultId) : null
    tabs = []
    active = null
    if (!vaultId) api.vaults().then((v) => (vaults = v)).catch(() => (vaults = []))
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
  let sidebar: 'files' | 'search' = $state('files')
  let switcher = $state(false)
  let backlinks: NoteSummary[] = $state([])

  function open(id: string) {
    if (!tabs.includes(id)) tabs = [...tabs, id]
    active = id
    switcher = false
    refreshBacklinks()
  }
  function close(id: string) {
    const i = tabs.indexOf(id)
    tabs = tabs.filter((t) => t !== id)
    if (active === id) active = tabs[Math.min(i, tabs.length - 1)] ?? null
  }
  function create(path: string) {
    if (!session) return
    open(session.createNote(path, `# ${displayName(path)}\n\n`))
  }
  function daily() {
    if (!session) return
    const d = new Date()
    const name = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
    const path = `Daily/${name}.md`
    const existing = session.idOf(path)
    open(existing ?? session.createNote(path, `# ${name}\n\n`))
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
    if (e.key === 'o' || e.key === 'p') {
      switcher = !switcher
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
        <button class:on={sidebar === 'files'} onclick={() => (sidebar = 'files')}>Files</button>
        <button class:on={sidebar === 'search'} onclick={() => (sidebar = 'search')}>Search</button>
        <span class="spacer"></span>
        <button title="New note (Ctrl+N)" onclick={() => (switcher = true)}>＋</button>
        <button title="Today's daily note (Ctrl+Shift+D)" onclick={daily}>📅</button>
      </div>
      {#if sidebar === 'files'}
        <Tree notes={session.notes} activeId={active} onOpen={open} />
      {:else}
        <SearchPane vault={session.id} onOpen={open} />
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
            <button onclick={renameActive} title="Rename / move">Rename</button>
            <button onclick={deleteActive} title="Move to trash">Delete</button>
          </div>
          <div class="editor-wrap">
            <Editor {session} noteId={active} onOpen={open} />
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
  .placeholder {
    display: grid;
    place-items: center;
    color: var(--muted);
  }
</style>
