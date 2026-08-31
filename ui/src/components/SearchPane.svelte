<script lang="ts">
  import { api, type SearchHit } from '../lib/api.ts'

  /**
   * Cross-vault search (SPEC §10): `/api/v1/search` ranks every vault the account can read
   * together, and the relay answers it for its one vault. Hits name a note id, which the shell
   * resolves back to a vault through the workspace.
   */
  let { label, onOpen }: { label?: (noteId: string) => string; onOpen: (id: string) => void } = $props()
  let query = $state('')
  let hits: SearchHit[] = $state([])
  let error = $state('')
  let timer: ReturnType<typeof setTimeout> | undefined

  function search() {
    clearTimeout(timer)
    timer = setTimeout(async () => {
      const q = query.trim()
      if (!q) {
        hits = []
        return
      }
      try {
        hits = await api.searchAll(q)
        error = ''
      } catch (e) {
        error = String(e)
      }
    }, 150)
  }
</script>

<div class="search">
  <input bind:value={query} oninput={search} placeholder="Search…" />
  {#if error}<p class="error">{error}</p>{/if}
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
