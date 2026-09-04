<script lang="ts" module>
  /** The app's glyphs — the sidebar's toolbar, the pane's chrome. Stroke-only paths on a 16×16 grid, so they take
   *  `currentColor` and stay legible next to 0.85rem text in either theme. */
  export type IconName =
    | 'expand'
    | 'collapse'
    | 'locate'
    | 'tree'
    | 'split'
    | 'subfolders'
    | 'vault'
    | 'plus'
    | 'indent'
    | 'outdent'
    | 'splitright'
    | 'closepane'
    | 'history'
    | 'star'

  const PATHS: Record<IconName, string[]> = {
    expand: ['M3.5 3.5 8 8l4.5-4.5', 'M3.5 9 8 13.5 12.5 9'],
    collapse: ['M3.5 7 8 2.5 12.5 7', 'M3.5 12.5 8 8l4.5 4.5'],
    locate: ['M8 5.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5Z', 'M8 1v2.2M8 12.8V15M1 8h2.2M12.8 8H15'],
    tree: ['M6 3.5h8', 'M6 8h8', 'M6 12.5h8', 'M2.5 2v9.5a1 1 0 0 0 1 1H4'],
    split: ['M2.5 3h11a.5.5 0 0 1 .5.5v9a.5.5 0 0 1-.5.5h-11a.5.5 0 0 1-.5-.5v-9a.5.5 0 0 1 .5-.5Z', 'M2 8h12'],
    subfolders: ['M4 3v5.5a2 2 0 0 0 2 2h6', 'M9.5 8 12 10.5 9.5 13'],
    vault: ['M2 5.5 8 2.5l6 3-6 3-6-3Z', 'M2 10.5 8 13.5l6-3'],
    plus: ['M8 3.5v9', 'M3.5 8h9'],
    splitright: ['M2.5 3h11a.5.5 0 0 1 .5.5v9a.5.5 0 0 1-.5.5h-11a.5.5 0 0 1-.5-.5v-9a.5.5 0 0 1 .5-.5Z', 'M8 3v10'],
    closepane: ['M4 4l8 8', 'M12 4l-8 8'],
    history: ['M8 2.3a5.7 5.7 0 1 0 0 11.4A5.7 5.7 0 0 0 8 2.3Z', 'M8 5.1V8l2 1.5'],
    star: ['M8 2.2 9.85 5.9l4.05.6-2.95 2.85.7 4.05L8 11.5l-3.65 1.9.7-4.05L2.1 6.5l4.05-.6L8 2.2Z'],
    indent: ['M2 3h12', 'M7 8h7', 'M2 13h12', 'M2 6l2.5 2L2 10'],
    outdent: ['M2 3h12', 'M7 8h7', 'M2 13h12', 'M4.5 6 2 8l2.5 2'],
  }
</script>

<script lang="ts">
  let { name, size = 14, filled = false }: { name: IconName; size?: number; filled?: boolean } = $props()
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 16 16"
  fill={filled ? 'currentColor' : 'none'}
  stroke="currentColor"
  stroke-width="1.4"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  {#each PATHS[name] as d (d)}<path {d} />{/each}
</svg>

<style>
  svg {
    flex: none;
    display: block;
  }
</style>
