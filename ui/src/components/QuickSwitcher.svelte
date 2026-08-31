<script lang="ts">
  import { displayName } from '../lib/vault.svelte.ts'
  import type { WorkspaceNote } from '../lib/workspace.svelte.ts'

  /** Fuzzy open across every vault (SPEC §9); a new note goes into `createVault`. */
  let {
    notes,
    label,
    createVault,
    onOpen,
    onCreate,
    onClose,
  }: {
    notes: WorkspaceNote[]
    /** Vault label to show beside a hit; empty when a single vault makes it noise. */
    label?: (vault: string) => string
    createVault?: string | null
    onOpen: (id: string) => void
    onCreate: (path: string) => void
    onClose: () => void
  } = $props()

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

  let canCreate = $derived(
    query.trim().length > 0 && !!createVault && !notes.some((n) => n.path === normalize(query)),
  )

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
  <div class="dialog" onmousedown={(e) => e.stopPropagation()} role="dialog" tabindex="-1" aria-label="Quick switcher">
    <input bind:this={input} bind:value={query} onkeydown={onKey} placeholder="Open or create a note…" />
    <ul>
      {#each results as n, i (n.id)}
        <li class:selected={i === selected}>
          <button onclick={() => choose(i)}>
            <span class="title">{displayName(n.path)}</span>
            <span class="path">{#if label?.(n.vault)}<span class="vault">{label(n.vault)}</span>{/if}{n.path}</span>
          </button>
        </li>
      {/each}
      {#if canCreate}
        <li class:selected={selected === results.length}>
          <button onclick={() => choose(results.length)}>
            <span class="title">Create “{normalize(query)}”</span>
            {#if createVault && label?.(createVault)}<span class="path"><span class="vault">{label(createVault)}</span></span>{/if}
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
    overflow: auto;
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
  .vault {
    text-transform: uppercase;
    font-size: 0.9em;
    letter-spacing: 0.03em;
    margin-right: 0.4em;
    opacity: 0.8;
  }
  .path {
    color: var(--muted);
    font-size: 0.85em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  /* A phone has no room to spare above an overlay, and a long one must be able to scroll. */
  @media (max-width: 720px) {
    .backdrop {
      padding-top: 5vh;
    }
  }

  /* Touch: a list row is a target, not a line of text. */
  @media (pointer: coarse) {
    li button {
      padding-top: 0.5rem;
      padding-bottom: 0.5rem;
    }
  }
</style>
