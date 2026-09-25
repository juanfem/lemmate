<script lang="ts">
  import { untrack } from 'svelte'
  import {
    DEFAULT_FOLDER,
    DEFAULT_FORMAT,
    DEFAULT_TEMPLATE,
    MONTHS,
    compare,
    daysInMonth,
    pathFor,
    today,
    weekday,
    type DailySettings,
    type Day,
  } from '../lib/daily.ts'

  // The daily-note calendar (SPEC §9): a month at a time, days that have a note marked, a click
  // opens that day's note (creating it from the template if it does not exist). Below it, the
  // vault's daily-note settings — shared by every replica, since they live in the vault doc.
  let {
    settings,
    exists,
    initial,
    onOpen,
    onSave,
    onClose,
  }: {
    settings: DailySettings
    /** Whether the vault holds a note at this path. */
    exists: (path: string) => boolean
    /** The month to show first. */
    initial?: Day | null
    onOpen: (day: Day) => void
    onSave: (s: DailySettings) => void
    onClose: () => void
  } = $props()

  const now = today()
  let shown = $state(untrack(() => ({ year: (initial ?? now).year, month: (initial ?? now).month })))
  let editing = $state(false)
  let draft = $state(untrack(() => ({ ...settings })))

  // Monday-first rows, blanks before the 1st.
  let cells = $derived.by(() => {
    const first: Day = { year: shown.year, month: shown.month, day: 1 }
    const lead = (weekday(first) + 6) % 7
    const out: (Day | null)[] = Array.from({ length: lead }, () => null)
    for (let d = 1; d <= daysInMonth(shown.year, shown.month); d++) out.push({ year: shown.year, month: shown.month, day: d })
    return out
  })
  function step(n: number) {
    const m = shown.month - 1 + n
    shown = { year: shown.year + Math.floor(m / 12), month: ((m % 12) + 12) % 12 + 1 }
  }
  let preview = $derived(pathFor(draft, now))
  function save() {
    onSave({ ...draft })
    editing = false
  }
  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') onClose()
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="backdrop" onmousedown={onClose} role="presentation">
  <div class="dialog" onmousedown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" aria-label="Daily notes" tabindex="-1">
    <header>
      <button class="nav" onclick={() => step(-1)} aria-label="Previous month">‹</button>
      <h2>{MONTHS[shown.month - 1]} {shown.year}</h2>
      <button class="nav" onclick={() => step(1)} aria-label="Next month">›</button>
    </header>
    <div class="grid" role="grid">
      {#each ['Mo', 'Tu', 'We', 'Th', 'Fr', 'Sa', 'Su'] as w (w)}<span class="wd">{w}</span>{/each}
      {#each cells as c, i (i)}
        {#if c}
          {@const has = exists(pathFor(settings, c))}
          <button
            class="day"
            class:has
            class:today={compare(c, now) === 0}
            title={pathFor(settings, c) + (has ? '' : ' (not created yet)')}
            onclick={() => onOpen(c)}
          >{c.day}</button>
        {:else}
          <span></span>
        {/if}
      {/each}
    </div>
    <div class="row">
      <button onclick={() => (shown = { year: now.year, month: now.month })}>This month</button>
      <button onclick={() => onOpen(now)}>Today</button>
      <span class="grow"></span>
      <button onclick={() => (editing = !editing)} aria-expanded={editing}>Settings</button>
    </div>
    {#if editing}
      <form class="settings" onsubmit={(e) => (e.preventDefault(), save())}>
        <label>Folder <span class="hint">(“/” for the vault root)</span>
          <input bind:value={draft.folder} placeholder={DEFAULT_FOLDER} />
        </label>
        <label>File name format <span class="hint">(Moment.js, as in Obsidian; may contain “/”)</span>
          <input bind:value={draft.format} placeholder={DEFAULT_FORMAT} />
        </label>
        <label>Template
          <input bind:value={draft.template} placeholder={DEFAULT_TEMPLATE} />
        </label>
        <p class="hint">Today’s note: <code>{preview}</code>. Existing notes are not moved.</p>
        <div class="row end">
          <button type="button" onclick={() => (draft = { folder: '', format: '', template: '' })}>Defaults</button>
          <button type="submit" class="primary">Save for this vault</button>
        </div>
      </form>
    {/if}
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgb(0 0 0 / 0.3); display: flex; align-items: flex-start; justify-content: center; padding-top: 12vh; z-index: 10; overflow: auto; }
  .dialog { width: min(22rem, 92vw); background: var(--panel); border: 1px solid var(--border); border-radius: 10px; box-shadow: 0 10px 40px rgb(0 0 0 / 0.3); padding: 0.9rem 1rem; display: flex; flex-direction: column; gap: 0.6rem; }
  header { display: flex; align-items: center; gap: 0.4rem; }
  h2 { margin: 0; font-size: 1rem; flex: 1; text-align: center; }
  .grid { display: grid; grid-template-columns: repeat(7, 1fr); gap: 0.15rem; }
  .wd { font-size: 0.7rem; color: var(--muted); text-align: center; padding-bottom: 0.2rem; }
  .day { font: inherit; font-size: 0.85rem; aspect-ratio: 1; border: 1px solid transparent; border-radius: 6px; background: none; color: inherit; cursor: pointer; position: relative; padding: 0; }
  .day:hover { background: var(--hover, rgb(127 127 127 / 0.12)); }
  .day.has::after { content: ''; position: absolute; left: 50%; bottom: 0.2rem; width: 0.3rem; height: 0.3rem; margin-left: -0.15rem; border-radius: 50%; background: var(--accent); }
  .day.today { border-color: var(--accent); font-weight: 600; }
  .row { display: flex; gap: 0.5rem; align-items: center; flex-wrap: wrap; }
  .row.end { justify-content: flex-end; }
  .grow { flex: 1; }
  .settings { display: flex; flex-direction: column; gap: 0.5rem; border-top: 1px solid var(--border); padding-top: 0.6rem; }
  label { display: flex; flex-direction: column; gap: 0.2rem; font-size: 0.85rem; color: var(--muted); }
  input { font: inherit; padding: 0.35rem 0.5rem; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: var(--fg, inherit); }
  button { font: inherit; font-size: 0.85rem; border: 1px solid var(--border); background: var(--bg); color: inherit; border-radius: 6px; padding: 0.3rem 0.65rem; cursor: pointer; }
  button.nav { padding: 0.2rem 0.6rem; font-size: 1rem; }
  button.day { border-color: transparent; background: none; }
  button.day.today { border-color: var(--accent); }
  button.primary { border-color: transparent; background: var(--accent); color: white; }
  .hint { color: var(--muted); font-size: 0.8rem; margin: 0; }
  code { font-family: var(--mono); font-size: 0.85em; word-break: break-all; }

  @media (max-width: 720px) {
    .backdrop { padding-top: 5vh; }
  }
</style>
