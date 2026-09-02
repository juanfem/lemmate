<script lang="ts">
  import { api, type SearchHit } from '../lib/api.ts'
  import { searchNotes, type IndexedNote } from '../lib/search.ts'
  import { loadVault } from '../lib/searchstore.ts'

  /**
   * Cross-vault search (SPEC §10): `/api/v1/search` ranks every vault the account can read
   * together, and the relay answers it for its one vault. Hits name a note id, which the shell
   * resolves back to a vault through the workspace.
   *
   * With no server, the offline index stands in — the notes an installed client has cached,
   * matched by lib/search.ts. It is weaker than SQLite's FTS and says so on screen, because a
   * result list that quietly stops being complete is worse than one that admits it.
   */
  let {
    label,
    onOpen,
    vaults = [],
  }: {
    label?: (noteId: string) => string
    onOpen: (id: string) => void
    /** Vault ids to search offline; the server needs no such hint. */
    vaults?: string[]
  } = $props()
  let query = $state('')
  let hits: SearchHit[] = $state([])
  let error = $state('')
  let offline = $state(false)
  let timer: ReturnType<typeof setTimeout> | undefined

  async function offlineSearch(q: string): Promise<SearchHit[]> {
    const notes: IndexedNote[] = []
    for (const vault of vaults) {
      for (const row of (await loadVault(vault)).values()) {
        notes.push({ id: row.id, vault: row.vault, title: row.title, text: row.text })
      }
    }
    if (notes.length === 0) throw new Error('nothing cached on this device')
    return searchNotes(notes, q)
  }

  function search() {
    clearTimeout(timer)
    timer = setTimeout(async () => {
      const q = query.trim()
      if (!q) {
        hits = []
        error = ''
        offline = false
        return
      }
      try {
        hits = await api.searchAll(q)
        error = ''
        offline = false
      } catch {
        // The server is the better answer whenever there is one, so this is a fallback rather
        // than a mode: no toggle, and it reverts the moment the network is back.
        try {
          hits = await offlineSearch(q)
          offline = true
          error = ''
        } catch (e) {
          hits = []
          offline = false
          error = `Search needs the server (${String(e)}).`
        }
      }
    }, 150)
  }
</script>

<div class="search">
  <input bind:value={query} oninput={search} placeholder="Search…" />
  {#if error}<p class="error">{error}</p>{/if}
  {#if offline}<p class="note">Offline — searching the {hits.length === 1 ? 'note' : 'notes'} cached on this device.</p>{/if}
  <ul>
    {#each hits as h (h.note_id)}
      <li>
        <button onclick={() => onOpen(h.note_id)}>
          <span class="title">{#if label?.(h.note_id)}<span class="vault">{label(h.note_id)}</span>{/if}{h.title ?? h.note_id}</span>
          <span class="snippet">{h.snippet}</span>
        </button>
      </li>
    {/each}
  </ul>
</div>

<style>
  .search {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  input {
    font: inherit;
    padding: 0.4rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--panel);
    color: inherit;
    margin: 0.4rem;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0 0.4rem;
    overflow: auto;
  }
  li button {
    display: block;
    width: 100%;
    text-align: left;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    padding: 0.4rem;
    border-radius: 6px;
    cursor: pointer;
  }
  li button:hover {
    background: var(--hover);
  }
  .title {
    display: block;
    font-weight: 600;
    font-size: 0.9rem;
  }
  .vault {
    color: var(--muted);
    text-transform: uppercase;
    font-size: 0.8em;
    letter-spacing: 0.03em;
    margin-right: 0.4em;
    font-weight: 400;
  }
  .snippet {
    display: block;
    color: var(--muted);
    font-size: 0.8rem;
  }
  .note {
    color: var(--muted);
    font-size: 0.78rem;
    margin: 0 0.5rem 0.2rem;
  }
  .error {
    color: #c33;
    padding: 0 0.6rem;
  }

  /* Touch: a list row is a target, not a line of text. */
  @media (pointer: coarse) {
    li button {
      padding-top: 0.5rem;
      padding-bottom: 0.5rem;
    }
  }
</style>
