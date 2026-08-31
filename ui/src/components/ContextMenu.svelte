<script lang="ts" module>
  export interface MenuItem {
    label: string
    run?: () => void
    /** Destructive: shown in red. */
    danger?: boolean
    disabled?: boolean
    /** A rule between groups; `label` is ignored. */
    separator?: boolean
  }

  export interface MenuState {
    x: number
    y: number
    items: MenuItem[]
  }

  /** Build a `MenuState` from a right-click, ready to assign to a `$state` slot. */
  export function menuAt(e: MouseEvent, items: MenuItem[]): MenuState {
    e.preventDefault()
    return { x: e.clientX, y: e.clientY, items }
  }
</script>

<script lang="ts">
  import { tick, untrack } from 'svelte'

  /** A right-click menu, positioned at the pointer and kept inside the viewport. */
  let { menu, onClose }: { menu: MenuState; onClose: () => void } = $props()

  let el: HTMLDivElement | undefined = $state()
  // Placed at the pointer, then nudged back inside the viewport once it has a size.
  let at = $state(untrack(() => ({ x: menu.x, y: menu.y })))
  let focused = $state(-1)

  let actionable = $derived(menu.items.map((it, i) => ({ it, i })).filter(({ it }) => !it.separator && !it.disabled))

  function choose(item: MenuItem) {
    if (item.disabled || item.separator) return
    onClose()
    item.run?.()
  }

  function step(delta: number) {
    if (actionable.length === 0) return
    const here = actionable.findIndex(({ i }) => i === focused)
    const next = actionable[(here + delta + actionable.length * 2) % actionable.length]
    focused = next?.i ?? -1
    ;(el?.querySelector(`[data-i="${focused}"]`) as HTMLElement | null)?.focus()
  }

  // Flip the menu back over the pointer when it would hang off the edge.
  $effect(() => {
    const node = el
    const { x, y } = menu
    if (!node) return
    void tick().then(() => {
      const w = node.offsetWidth
      const h = node.offsetHeight
      at = {
        x: x + w > window.innerWidth - 8 ? Math.max(8, x - w) : x,
        y: y + h > window.innerHeight - 8 ? Math.max(8, y - h) : y,
      }
      node.focus()
    })
  })
</script>

<svelte:window
  onkeydown={(e) => {
    if (e.key === 'Escape') (e.preventDefault(), onClose())
    else if (e.key === 'ArrowDown') (e.preventDefault(), step(1))
    else if (e.key === 'ArrowUp') (e.preventDefault(), step(-1))
  }}
  onresize={onClose}
  onblur={onClose}
/>
<!-- Anything outside dismisses; capture so a click never reaches what is underneath. -->
<div class="scrim" onpointerdown={onClose} oncontextmenu={(e) => (e.preventDefault(), onClose())} role="presentation"></div>
<div class="menu" bind:this={el} style:left="{at.x}px" style:top="{at.y}px" role="menu" tabindex="-1">
  {#each menu.items as item, i (i)}
    {#if item.separator}
      <hr />
    {:else}
      <button
        role="menuitem"
        data-i={i}
        class:danger={item.danger}
        disabled={item.disabled}
        onclick={() => choose(item)}
        onmouseenter={() => (focused = i)}
      >
        {item.label}
      </button>
    {/if}
  {/each}
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 60;
  }
  .menu {
    position: fixed;
    z-index: 61;
    min-width: 12rem;
    max-width: 20rem;
    padding: 0.25rem;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: 0 8px 28px rgb(0 0 0 / 0.22);
    display: flex;
    flex-direction: column;
  }
  .menu:focus {
    outline: none;
  }
  button {
    font: inherit;
    font-size: 0.85rem;
    text-align: left;
    border: 0;
    background: none;
    color: inherit;
    padding: 0.35rem 0.6rem;
    border-radius: 5px;
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  button:hover:not(:disabled),
  button:focus-visible {
    background: var(--hover);
    outline: none;
  }
  button:disabled {
    color: var(--muted);
    cursor: default;
  }
  button.danger {
    color: #dc2626;
  }
  button.danger:hover:not(:disabled) {
    background: #dc26261a;
  }
  hr {
    border: 0;
    border-top: 1px solid var(--border);
    margin: 0.25rem 0.3rem;
  }
</style>
