<script lang="ts">
  import { untrack } from 'svelte'

  // Folding one vault into another (SPEC §3.2): the notes of one vault end up inside another,
  // keeping their ids, their history and their images, and the vault they came from stops
  // existing — here, and on the server if it had one.
  //
  // Nothing happens until the plan has been shown. The relay works the plan out for a dry run
  // and again for the real thing, so what you approve is what runs.
  let {
    vaults,
    initialFrom,
    onClose,
  }: {
    vaults: { id: string; label: string; notes: number }[]
    initialFrom?: string | null
    onClose: () => void
  } = $props()

  type Planned = { from: string; to: string; renamed: boolean }
  type PlannedAttachment = { from: string; to: string; fate: 'new' | 'same' | 'renamed' }
  type Plan = { folder: string; notes: Planned[]; attachments: PlannedAttachment[] }

  // The list does not change while the dialog is open; these only seed the two dropdowns.
  let from = $state(untrack(() => initialFrom ?? vaults[0]?.id ?? ''))
  let into = $state(untrack(() => vaults.find((v) => v.id !== (initialFrom ?? vaults[0]?.id))?.id ?? ''))
  // Null until edited, so the relay's default (the source vault's name) is used as the folder.
  let folder = $state<string | null>(null)
  let plan = $state<Plan | null>(null)
  let error = $state('')
  let busy = $state(false)

  let label = (id: string) => vaults.find((v) => v.id === id)?.label ?? id.slice(-6)
  let destinations = $derived(vaults.filter((v) => v.id !== from))
  $effect(() => {
    // Changing either side invalidates a plan drawn for the old pair.
    void from
    void into
    plan = null
  })

  async function ask(dry: boolean) {
    busy = true
    error = ''
    try {
      const r = await fetch('/api/v1/local/merge', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ from, into, folder, dry_run: dry }),
      })
      if (!r.ok) throw new Error((await r.text()).trim() || `${r.status} ${r.statusText}`)
      const body = (await r.json()) as { plan: Plan; applied: boolean; left: string[]; folder_removed: boolean }
      if (dry) {
        plan = body.plan
        // Show the folder the relay chose, so the field says what will happen.
        folder = body.plan.folder
      } else {
        // Vaults, sessions and the tree all change at once; the cheapest correct answer is to
        // come back up on the new shape.
        location.reload()
      }
    } catch (e) {
      error = String(e instanceof Error ? e.message : e)
    } finally {
      busy = false
    }
  }

  let renamedNotes = $derived(plan?.notes.filter((n) => n.renamed).length ?? 0)
  let copiedAttachments = $derived(plan?.attachments.filter((a) => a.fate !== 'same').length ?? 0)
  let renamedAttachments = $derived(plan?.attachments.filter((a) => a.fate === 'renamed').length ?? 0)
</script>

<div class="backdrop" onmousedown={() => (busy ? null : onClose())} role="presentation">
  <div class="dialog" onmousedown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" aria-label="Merge vaults" tabindex="-1">
    <h2>Merge a vault into another</h2>
    <div class="row">
      <label>Move everything from <select bind:value={from}>{#each vaults as v (v.id)}<option value={v.id}>{v.label} ({v.notes})</option>{/each}</select></label>
      <label>into <select bind:value={into}>{#each destinations as v (v.id)}<option value={v.id}>{v.label} ({v.notes})</option>{/each}</select></label>
    </div>
    <label>Folder inside {label(into)} <span class="hint">(empty merges at its root)</span>
      <input value={folder ?? label(from)} oninput={(e) => (folder = e.currentTarget.value)} />
    </label>

    {#if plan}
      <div class="plan">
        <p>
          <strong>{plan.notes.length}</strong> {plan.notes.length === 1 ? 'note' : 'notes'} → <code>{plan.folder || '(root)'}</code>
          {#if renamedNotes}, <strong>{renamedNotes}</strong> renamed to avoid a name the destination already uses{/if}
        </p>
        {#if plan.attachments.length}
          <p>{copiedAttachments} of {plan.attachments.length} attachments copied{#if renamedAttachments}, {renamedAttachments} renamed (the notes that point at them are rewritten){/if}.</p>
        {/if}
        <ul>
          {#each plan.notes.slice(0, 8) as n (n.from)}
            <li><code>{n.from}</code> → <code>{n.to}</code>{#if n.renamed}<span class="hint"> renamed</span>{/if}</li>
          {/each}
          {#if plan.notes.length > 8}<li class="hint">…and {plan.notes.length - 8} more</li>{/if}
        </ul>
        <p class="warn">
          The <strong>{label(from)}</strong> vault is then removed from this computer — and from the server, if it is on one. Its notes are
          not deleted: they keep their ids and their history inside {label(into)}.
        </p>
      </div>
    {/if}

    {#if error}<p class="error">{error}</p>{/if}
    <div class="row end">
      <button onclick={onClose} disabled={busy}>Cancel</button>
      {#if plan}
        <button class="primary danger" onclick={() => ask(false)} disabled={busy}>{busy ? 'Merging…' : `Merge into ${label(into)}`}</button>
      {:else}
        <button class="primary" onclick={() => ask(true)} disabled={busy || !from || !into}>{busy ? 'Checking…' : 'Show what will happen'}</button>
      {/if}
    </div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgb(0 0 0 / 0.3); display: flex; align-items: flex-start; justify-content: center; padding-top: 12vh; z-index: 10; overflow: auto; }
  .dialog { width: min(36rem, 92vw); background: var(--panel); border: 1px solid var(--border); border-radius: 10px; box-shadow: 0 10px 40px rgb(0 0 0 / 0.3); padding: 1rem 1.2rem; display: flex; flex-direction: column; gap: 0.7rem; }
  h2 { margin: 0; font-size: 1.05rem; }
  .row { display: flex; gap: 0.6rem; align-items: end; flex-wrap: wrap; }
  .row.end { justify-content: flex-end; }
  label { display: flex; flex-direction: column; gap: 0.2rem; font-size: 0.85rem; color: var(--muted); flex: 1; }
  select, input { font: inherit; padding: 0.35rem 0.5rem; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: inherit; }
  button { font: inherit; font-size: 0.9rem; border: 1px solid var(--border); background: var(--bg); color: inherit; border-radius: 6px; padding: 0.35rem 0.7rem; cursor: pointer; }
  button.primary { border-color: transparent; background: var(--accent); color: white; }
  button.primary.danger { background: #dc2626; }
  button:disabled { opacity: 0.6; cursor: default; }
  .plan { border: 1px solid var(--border); border-radius: 8px; padding: 0.6rem 0.8rem; display: flex; flex-direction: column; gap: 0.4rem; max-height: 40vh; overflow: auto; }
  .plan p { margin: 0; font-size: 0.9rem; }
  ul { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 0.15rem; font-size: 0.85rem; }
  code { font-family: var(--mono); font-size: 0.85em; }
  .hint { color: var(--muted); font-size: 0.85em; }
  .warn { color: var(--muted); font-size: 0.85rem; line-height: 1.4; }
  .error { color: #dc2626; margin: 0; font-size: 0.85rem; white-space: pre-wrap; }

  /* A phone has no room to spare above an overlay, and a long one must be able to scroll. */
  @media (max-width: 720px) {
    .backdrop { padding-top: 5vh; }
  }
</style>
