<script lang="ts">
  import { SvelteSet } from 'svelte/reactivity'
  import { blocks, inlineSpan, lineDiff, type DiffLine } from '../lib/diff.ts'

  /**
   * Two versions of a note as one unified diff: what went (struck, red), what came (green), and
   * between the changes only a few lines of what stayed — the rest folds away behind a line
   * that says how much, so a long note shows its changes rather than its length. The bar on
   * top steps from one change to the next.
   */
  let { before, after }: { before: string; after: string } = $props()

  /** Unchanged lines kept on each side of a change. */
  const CONTEXT = 3
  /** Fewer lines than this are shown rather than folded: the fold line would be as long. */
  const MIN_FOLD = 4

  let diff = $derived(lineDiff(before, after))
  let parts = $derived(blocks(diff))
  let added = $derived(diff.reduce((n, l) => n + +(l.kind === 'add'), 0))
  let removed = $derived(diff.reduce((n, l) => n + +(l.kind === 'del'), 0))
  /** For each block, its number among the changes (-1 for a shared run). */
  let ordinal = $derived.by(() => {
    let n = 0
    return parts.map((p) => (p.type === 'change' ? n++ : -1))
  })
  let count = $derived(ordinal.reduce((n, o) => Math.max(n, o + 1), 0))

  /** Shared runs unfolded by hand, by block index. */
  const opened = new SvelteSet<number>()
  let at = $state(-1)
  const els: HTMLElement[] = $state([])

  $effect(() => {
    void [before, after]
    opened.clear()
    at = -1
  })

  function go(to: number) {
    at = Math.max(0, Math.min(count - 1, to))
    els[at]?.scrollIntoView({ block: 'start', behavior: 'smooth' })
  }

  /** Which lines of the shared run at block `i` are on show, and how many are folded. */
  function visible(i: number, run: DiffLine[]) {
    const head = i === 0 ? 0 : CONTEXT
    const tail = i === parts.length - 1 ? 0 : CONTEXT
    if (opened.has(i) || run.length < head + tail + MIN_FOLD) return { head: run, hidden: 0, tail: [] }
    return { head: run.slice(0, head), hidden: run.length - head - tail, tail: run.slice(run.length - tail) }
  }

  /** Paired lines of a one-for-one replacement, each with the span that differs. */
  function spans(del: DiffLine[], add: DiffLine[]): ([number, number, number] | null)[] {
    if (del.length !== add.length) return []
    return del.map((d, k) => inlineSpan(d.text, add[k]!.text))
  }

  const blank = (t: string) => t || '​'
</script>

{#snippet line(l: DiffLine, mark: [number, number] | null)}
  <div class="line {l.kind}">
    <span class="sign" aria-hidden="true">{l.kind === 'add' ? '+' : l.kind === 'del' ? '−' : ''}</span>
    {#if mark && mark[1] > mark[0]}
      <span class="text">{l.text.slice(0, mark[0])}<mark>{l.text.slice(mark[0], mark[1])}</mark>{l.text.slice(mark[1])}</span>
    {:else}
      <span class="text">{blank(l.text)}</span>
    {/if}
  </div>
{/snippet}

<div class="diff">
  <div class="bar">
    {#if count === 0}
      <span class="none">No differences.</span>
    {:else}
      <span class="add-n">+{added}</span>
      <span class="del-n">−{removed}</span>
      <span class="count">{count === 1 ? '1 change' : `${count} changes`}</span>
      <span class="grow"></span>
      {#if at >= 0}<span class="where">{at + 1} of {count}</span>{/if}
      <button onclick={() => go(at - 1)} disabled={at <= 0} title="Previous change" aria-label="Previous change">↑</button>
      <button onclick={() => go(at + 1)} disabled={at >= count - 1} title="Next change" aria-label="Next change">↓</button>
    {/if}
  </div>
  {#if count > 0}
    <div class="body">
      {#each parts as part, i (i)}
        {#if part.type === 'same'}
          {@const v = visible(i, part.lines)}
          {#each v.head as l (l.a)}{@render line(l, null)}{/each}
          {#if v.hidden > 0}
            <button class="fold" onclick={() => opened.add(i)}>
              ⋯ {v.hidden === 1 ? '1 unchanged line' : `${v.hidden} unchanged lines`}
            </button>
          {/if}
          {#each v.tail as l (l.a)}{@render line(l, null)}{/each}
        {:else}
          {@const pairs = spans(part.del, part.add)}
          <div class="change" class:current={ordinal[i] === at} bind:this={els[ordinal[i]!]}>
            {#each part.del as l, k (l.a)}{@render line(l, pairs[k] ? [pairs[k]![0], pairs[k]![1]] : null)}{/each}
            {#each part.add as l, k (l.b)}{@render line(l, pairs[k] ? [pairs[k]![0], pairs[k]![2]] : null)}{/each}
          </div>
        {/if}
      {/each}
    </div>
  {/if}
</div>

<style>
  .diff {
    width: 100%;
    max-width: var(--measure);
    margin: 0 auto;
    padding: 0 clamp(0.9rem, 4vw, 2.5rem) 3rem;
  }
  .bar {
    position: sticky;
    top: 0;
    z-index: 1;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.45rem 0;
    background: var(--bg);
    border-bottom: 1px solid var(--border-soft);
    font-size: 0.75rem;
    color: var(--faint);
    font-variant-numeric: tabular-nums;
  }
  .add-n {
    color: var(--ok);
    font-weight: 600;
  }
  .del-n {
    color: var(--danger);
    font-weight: 600;
  }
  .grow {
    flex: 1;
  }
  .bar button {
    font: inherit;
    border: 1px solid var(--border);
    background: var(--bg);
    color: var(--fg);
    border-radius: 6px;
    width: 1.9rem;
    padding: 0.15rem 0;
    cursor: pointer;
  }
  .bar button:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .body {
    margin-top: 0.6rem;
    font-family: var(--mono);
    font-size: 0.8rem;
    line-height: 1.55;
  }
  .line {
    display: flex;
    gap: 0.5rem;
    padding: 0 0.4rem;
  }
  .sign {
    flex: none;
    width: 0.7rem;
    color: var(--faint);
    user-select: none;
  }
  .text {
    min-width: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    color: var(--muted);
  }
  .add {
    background: color-mix(in srgb, var(--ok) 13%, transparent);
  }
  .add .text {
    color: var(--fg);
  }
  .add .sign {
    color: var(--ok);
  }
  .del {
    background: color-mix(in srgb, var(--danger) 9%, transparent);
  }
  .del .text {
    color: var(--fg);
    text-decoration: line-through;
    text-decoration-color: color-mix(in srgb, var(--danger) 45%, transparent);
  }
  .del .sign {
    color: var(--danger);
  }
  mark {
    color: inherit;
    border-radius: 2px;
  }
  .add mark {
    background: color-mix(in srgb, var(--ok) 32%, transparent);
  }
  .del mark {
    background: color-mix(in srgb, var(--danger) 24%, transparent);
  }
  .change {
    margin: 0.15rem 0;
    border-radius: 4px;
    overflow: hidden;
    /* Clear of the sticky bar when a change is scrolled to. */
    scroll-margin-top: 3rem;
  }
  .change.current {
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .fold {
    display: block;
    width: 100%;
    margin: 0.25rem 0;
    padding: 0.2rem 0.4rem;
    text-align: left;
    font: inherit;
    font-family: var(--ui);
    font-size: 0.72rem;
    color: var(--faint);
    background: var(--panel);
    border: 0;
    border-radius: 4px;
    cursor: pointer;
  }
  .fold:hover {
    color: var(--muted);
    background: var(--hover);
  }
  .none {
    color: var(--faint);
  }
</style>
