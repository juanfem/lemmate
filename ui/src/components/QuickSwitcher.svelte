<script lang="ts">
  import type { NoteEntry } from '../lib/vault.svelte.ts'
  import { displayName } from '../lib/vault.svelte.ts'

  let {
    notes,
    onOpen,
    onCreate,
    onClose,
  }: { notes: NoteEntry[]; onOpen: (id: string) => void; onCreate: (path: string) => void; onClose: () => void } = $props()

  let query = $state('')
  let selected = $state(0)
  let input: HTMLInputElement

  function score(path: string, q: string): number {
    const p = path.toLowerCase()
    if (p.includes(q)) return 100 - p.indexOf(q) - (p.length - q.length) / 100
    // subsequence match
    let i = 0
    for (const ch of p) if (ch === q[i]) i++
    return i === q.length ? 50 - p.length / 100 : -1
  }

  let results = $derived.by(() => {
    const q = query.trim().toLowerCase()
    if (!q) return notes.slice(0, 50)
    return notes
      .map((n) => ({ n, s: score(n.path, q) }))
      .filter((x) => x.s >= 0)
      .sort((a, b) => b.s - a.s)
      .slice(0, 50)
      .map((x) => x.n)
  })

  let canCreate = $derived(query.trim().length > 0 && !notes.some((n) => n.path === normalize(query)))

  function normalize(q: string): string {
    const t = q.trim().replace(/^\/+/u, '')
    return t.endsWith('.md') || t.endsWith('.qmd') ? t : `${t}.md`
  }

  function choose(i: number) {
    if (i < results.length) onOpen(results[i]!.id)
    else if (canCreate) onCreate(normalize(query))
  }

  function onKey(e: KeyboardEvent) {
    const total = results.length + (canCreate ? 1 : 0)
    if (e.key === 'ArrowDown') {
      selected = (selected + 1) % Math.max(total, 1)
      e.preventDefault()
    } else if (e.key === 'ArrowUp') {
      selected = (selected - 1 + Math.max(total, 1)) % Math.max(total, 1)
      e.preventDefault()
    } else if (e.key === 'Enter') {
      choose(selected)
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
</script>

<div class="backdrop" onmousedown={onClose} role="presentation">
  <div class="dialog" onmousedown={(e) => e.stopPropagation()} role="dialog" aria-label="Quick switcher">
    <input bind:this={input} bind:value={query} onkeydown={onKey} placeholder="Open or create a note…" />
    <ul>
      {#each results as n, i (n.id)}
        <li class:selected={i === selected}>
          <button onclick={() => choose(i)}>
            <span class="title">{displayName(n.path)}</span>
            <span class="path">{n.path}</span>
          </button>
        </li>
      {/each}
      {#if canCreate}
        <li class:selected={selected === results.length}>
          <button onclick={() => choose(results.length)}>
            <span class="title">Create “{normalize(query)}”</span>
          </button>
        </li>
      {/if}
    </ul>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 0.3);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding-top: 12vh;
    z-index: 10;
  }
  .dialog {
    width: min(40rem, 90vw);
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: 0 10px 40px rgb(0 0 0 / 0.3);
    overflow: hidden;
  }
  input {
    width: 100%;
    font: inherit;
    font-size: 1.1rem;
    padding: 0.8rem 1rem;
    border: 0;
    border-bottom: 1px solid var(--border);
    background: transparent;
    color: inherit;
    outline: none;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0.3rem;
    max-height: 50vh;
    overflow: auto;
  }
  li button {
    width: 100%;
    display: flex;
    justify-content: space-between;
    gap: 1rem;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    padding: 0.4rem 0.7rem;
    border-radius: 6px;
    text-align: left;
    cursor: pointer;
  }
  li.selected button,
  li button:hover {
    background: var(--accent-bg);
  }
  .path {
    color: var(--muted);
    font-size: 0.85em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
