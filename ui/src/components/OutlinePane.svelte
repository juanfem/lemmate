<script lang="ts">
  export interface OutlineItem {
    level: number
    text: string
    pos: number
  }
  let { items, onJump }: { items: OutlineItem[]; onJump: (pos: number) => void } = $props()
</script>

<nav class="outline">
  {#each items as h, i (i)}
    <button style:padding-left="{(h.level - 1) * 0.8 + 0.5}rem" onclick={() => onJump(h.pos)} title={h.text}>{h.text}</button>
  {/each}
  {#if items.length === 0}<p class="empty">No headings.</p>{/if}
</nav>

<style>
  .outline { display: flex; flex-direction: column; overflow: auto; padding: 0.3rem 0; }
  button { font: inherit; font-size: 0.85rem; text-align: left; border: 0; background: none; color: inherit; padding: 0.2rem 0.5rem; cursor: pointer; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; border-radius: 4px; }
  button:hover { background: var(--hover); }
  .empty { color: var(--muted); padding: 0.5rem; font-size: 0.85rem; }
</style>
