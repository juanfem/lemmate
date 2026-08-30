<script lang="ts">
  export interface Command {
    id: string
    label: string
    shortcut?: string
    run: () => void
  }
  let { commands, onClose }: { commands: Command[]; onClose: () => void } = $props()
  let query = $state('')
  let selected = $state(0)
  let input: HTMLInputElement

  let results = $derived.by(() => {
    const q = query.trim().toLowerCase()
    return q ? commands.filter((c) => c.label.toLowerCase().includes(q)) : commands
  })
  function run(i: number) {
    const c = results[i]
    if (!c) return
    onClose()
    c.run()
  }
  function onKey(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') { selected = (selected + 1) % Math.max(results.length, 1); e.preventDefault() }
    else if (e.key === 'ArrowUp') { selected = (selected - 1 + Math.max(results.length, 1)) % Math.max(results.length, 1); e.preventDefault() }
    else if (e.key === 'Enter') { run(selected); e.preventDefault() }
    else if (e.key === 'Escape') onClose()
  }
  $effect(() => { input?.focus() })
  $effect(() => { query; selected = 0 })
</script>

<div class="backdrop" onmousedown={onClose} role="presentation">
  <div class="dialog" onmousedown={(e) => e.stopPropagation()} role="dialog" aria-label="Command palette" tabindex="-1">
    <input bind:this={input} bind:value={query} onkeydown={onKey} placeholder="Type a command…" />
    <ul>
      {#each results as c, i (c.id)}
        <li class:selected={i === selected}>
          <button onclick={() => run(i)}><span>{c.label}</span>{#if c.shortcut}<kbd>{c.shortcut}</kbd>{/if}</button>
        </li>
      {/each}
    </ul>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgb(0 0 0 / 0.3); display: flex; align-items: flex-start; justify-content: center; padding-top: 12vh; z-index: 10; }
  .dialog { width: min(36rem, 90vw); background: var(--panel); border: 1px solid var(--border); border-radius: 10px; box-shadow: 0 10px 40px rgb(0 0 0 / 0.3); overflow: hidden; }
  input { width: 100%; font: inherit; font-size: 1.05rem; padding: 0.7rem 1rem; border: 0; border-bottom: 1px solid var(--border); background: transparent; color: inherit; outline: none; }
  ul { list-style: none; margin: 0; padding: 0.3rem; max-height: 50vh; overflow: auto; }
  li button { width: 100%; display: flex; justify-content: space-between; align-items: center; border: 0; background: none; color: inherit; font: inherit; padding: 0.4rem 0.7rem; border-radius: 6px; text-align: left; cursor: pointer; }
  li.selected button, li button:hover { background: var(--accent-bg); }
  kbd { color: var(--muted); }
</style>
