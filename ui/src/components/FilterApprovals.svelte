<script lang="ts">
  import { api, ApiError, type FilterApproval } from '../lib/api.ts'
  import type { Unapproved } from '../lib/render.ts'
  import { trapFocus } from '../lib/focustrap.ts'
  import DiffView from './DiffView.svelte'

  /**
   * The Lua filters a vault's renders may run (SPEC §5.6). Opened from a render that named some
   * nobody has approved, it shows each one's code — against the version approved before, when
   * the same file had one — to approve; opened on its own, it lists what is approved, to revoke.
   * An approval names these exact bytes: one changed later is a new file to review.
   */
  let {
    vault,
    label,
    review = [],
    onApproved,
    onClose,
  }: {
    vault: string
    label: string
    /** Files a render did not run, to review. */
    review?: Unapproved[]
    /** Something was approved: the render that asked can run again. */
    onApproved?: () => void
    onClose: () => void
  } = $props()

  let approvals: FilterApproval[] = $state([])
  let loaded = $state(false)
  let error = $state('')
  /** The code of each file under review, and of the version of it approved before, by hash. */
  let code: Record<string, string | null> = $state({})
  let done: Record<string, boolean> = $state({})

  /** What a refusal means here (`approve_filter` / `revoke_filter`, server and relay). */
  function explain(e: unknown, what: 'approve' | 'revoke'): string {
    if (!(e instanceof ApiError)) return String(e)
    if (e.status === 403) {
      return what === 'approve'
        ? 'Only an owner of this vault, signed in to the web app, can approve filters for everyone.'
        : 'This approval is the server’s: revoke it there, signed in as an owner of the vault.'
    }
    if (e.status === 409) return 'The file has changed since this render. Render again, and review what it is now.'
    if (e.status === 404) return what === 'approve' ? 'That file is no longer in the vault.' : 'That approval is already gone.'
    if (e.status === 502) return 'The server could not be reached to revoke its approval.'
    return String(e)
  }

  async function reload() {
    try {
      approvals = await api.filterApprovals(vault)
    } catch (e) {
      error = e instanceof ApiError && e.status === 404 ? 'Filters cannot be approved here.' : String(e)
    }
    loaded = true
  }

  /** The approval this file replaces: the newest one of the same path, if any. */
  function earlier(u: Unapproved): FilterApproval | undefined {
    return approvals.find((a) => a.path === u.path && a.hash !== u.hash)
  }

  $effect(() => {
    void (async () => {
      await reload()
      const wanted = new Set(review.map((u) => u.hash))
      for (const u of review) {
        const before = earlier(u)
        if (before) wanted.add(before.hash)
      }
      for (const hash of wanted) {
        api.fileText(vault, hash).then((t) => (code[hash] = t), () => (code[hash] = null))
      }
    })()
  })

  async function approve(u: Unapproved) {
    error = ''
    try {
      await api.approveFilter(vault, u.path, u.hash)
      done[u.hash] = true
      await reload()
      onApproved?.()
    } catch (e) {
      error = explain(e, 'approve')
    }
  }

  async function revoke(a: FilterApproval) {
    error = ''
    try {
      await api.revokeFilter(vault, a.hash)
    } catch (e) {
      error = explain(e, 'revoke')
    }
    await reload()
  }

  const when = (ms: number) => new Date(ms).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' })
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onClose()} />

<div class="backdrop" onmousedown={onClose} role="presentation">
  <div class="dialog" use:trapFocus onmousedown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" aria-label="Lua filters" tabindex="-1">
    <h2>{review.length ? 'Lua filters this note wants to run' : `Approved Lua filters in “${label}”`}</h2>
    <p class="muted">
      A filter is a program. Once approved, it runs whenever a note in this vault names it and is rendered — confined: it
      reaches no network and no file outside the render. Approve only code you have read. A single changed byte makes it a new
      file, to be approved again.
    </p>
    {#if error}<p class="error">{error}</p>{/if}

    {#each review as u (u.hash)}
      {@const before = earlier(u)}
      <section>
        <div class="row">
          <code class="path">{u.path}</code>
          {#if done[u.hash]}
            <span class="ok">Approved</span>
          {:else}
            <button class="primary" onclick={() => approve(u)} disabled={code[u.hash] === undefined}>Approve</button>
          {/if}
        </div>
        {#if code[u.hash] === undefined}
          <p class="muted">Loading…</p>
        {:else if code[u.hash] === null}
          <p class="muted">The file cannot be read here — it may have changed since the render.</p>
        {:else if before && typeof code[before.hash] === 'string'}
          <p class="muted">Changes since the version {before.approved_by} approved on {when(before.approved_ms)}:</p>
          <div class="code diff"><DiffView before={code[before.hash] as string} after={code[u.hash] as string} /></div>
        {:else}
          <pre class="code">{code[u.hash]}</pre>
        {/if}
      </section>
    {/each}

    {#if loaded && (approvals.length || !review.length)}
      <h3>{review.length ? 'Approved already' : 'Approved'}</h3>
      {#if approvals.length}
        <ul>
          {#each approvals as a (a.hash)}
            <li>
              <span class="who">
                <code class="path">{a.path}</code>
                <span class="muted">
                  {a.approved_by}, {when(a.approved_ms)}{#if a.source === 'device'} · on this device only{/if}
                  · <span class="hash" title={a.hash}>{a.hash.slice(0, 12)}</span>
                </span>
              </span>
              <button onclick={() => revoke(a)}>Revoke</button>
            </li>
          {/each}
        </ul>
      {:else}
        <p class="muted">None. A note that names a Lua filter offers it for review when it is rendered.</p>
      {/if}
    {/if}
    <div class="row end"><button onclick={onClose}>Done</button></div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgb(0 0 0 / 0.3); display: flex; align-items: flex-start; justify-content: center; padding-top: 8vh; z-index: 10; overflow: auto; }
  .dialog { width: min(48rem, 94vw); max-height: 84vh; overflow: auto; background: var(--panel); border: 1px solid var(--border); border-radius: 10px; box-shadow: 0 10px 40px rgb(0 0 0 / 0.3); padding: 1rem 1.2rem; display: flex; flex-direction: column; gap: 0.6rem; }
  h2 { margin: 0; font-size: 1.05rem; }
  h3 { margin: 0.4rem 0 0; font-size: 0.9rem; }
  section { display: flex; flex-direction: column; gap: 0.35rem; border-top: 1px solid var(--border-soft); padding-top: 0.6rem; }
  .row { display: flex; gap: 0.4rem; align-items: center; }
  .row.end { justify-content: flex-end; }
  .path { flex: 1; min-width: 0; overflow-wrap: anywhere; font-size: 0.85rem; }
  .code { margin: 0; max-height: 24rem; overflow: auto; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); }
  pre.code { padding: 0.5rem 0.7rem; font-size: 0.8rem; white-space: pre; }
  button { font: inherit; font-size: 0.9rem; border: 1px solid var(--border); background: var(--bg); color: inherit; border-radius: 6px; padding: 0.35rem 0.7rem; cursor: pointer; }
  button.primary { border-color: var(--accent, #2563eb); }
  button:disabled { opacity: 0.5; cursor: default; }
  ul { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 0.3rem; }
  li { display: flex; gap: 0.4rem; align-items: center; font-size: 0.9rem; }
  .who { flex: 1; min-width: 0; display: flex; flex-direction: column; }
  .hash { font-family: var(--mono, monospace); }
  .ok { color: #16a34a; font-size: 0.85rem; }
  .muted { color: var(--muted); font-size: 0.8rem; margin: 0; }
  .error { color: #dc2626; margin: 0; font-size: 0.85rem; }

  @media (max-width: 720px) {
    .backdrop {
      padding-top: 3vh;
    }
  }
</style>
