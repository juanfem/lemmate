<script lang="ts">
  import { api, type SearchHit } from '../lib/api.ts'

  let { vault, onOpen }: { vault: string; onOpen: (id: string) => void } = $props()
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
        hits = await api.search(vault, q)
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
          <span class="title">{h.title ?? h.note_id}</span>
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
  .snippet {
    display: block;
    color: var(--muted);
    font-size: 0.8rem;
  }
  .error {
    color: #c33;
    padding: 0 0.6rem;
  }
</style>
