/**
 * What to call a note whose id resolves to no path.
 *
 * "(deleted)" is a claim about a vault's note list, and a client that has not got one yet is in
 * no position to make it: a cold start — a reload, or a PWA the phone killed in the background —
 * restores its tabs from `localStorage` before either copy of the vault doc, the offline cache's
 * or the server's, has arrived. Saying "unknown" for a moment is right; saying "deleted" about a
 * note that is sitting there is not, and it is the state a phone comes back to.
 */
export function unnamedNote(vault: { noteOnly: boolean; vaultLoaded: boolean } | undefined): string {
  // A directly shared note (SPEC §11.2) is granted without its vault, so there is no list to be
  // missing from and never will be: its path is not something this client is entitled to know.
  if (vault?.noteOnly) return 'shared note'
  return vault?.vaultLoaded ? '(deleted)' : ''
}

/**
 * The path a note typed by name gets: leading slashes off, and `.md` on unless it already ends in
 * `.md` or `.qmd`. Every way of making a note by name goes through here — the palette, and the
 * folder and vault "new note" prompts, which once wrote `Work/Delta` with no extension at all and
 * so made a file nothing else recognised as a note.
 */
export function notePath(text: string): string {
  const t = text.trim().replace(/^\/+/u, '')
  return t.endsWith('.md') || t.endsWith('.qmd') ? t : `${t}.md`
}

const RESERVED = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/iu

/**
 * A vault path every replica can hold. The server refuses a vault-doc update that brings in a
 * hidden, absolute or climbing path — and refuses the *whole* frame, so one such entry would
 * leave this client's vault doc refused on every reconnect — and a desktop replica writes no
 * file for a name Windows could not hold (`projection::check_path`). So a path is made safe
 * here, where it enters the vault doc, rather than refused later: separators normalised, empty,
 * `.` and `..` segments dropped, leading dots and trailing dots or spaces trimmed, the
 * characters Windows forbids turned into `-`, and a reserved device name given a `_`.
 */
export function safeVaultPath(path: string): string {
  const segs = path
    .replace(/\\/gu, '/')
    .split('/')
    .map((seg) =>
      seg
        // eslint-disable-next-line no-control-regex
        .replace(/[\u0000-\u001f\u007f]/gu, '')
        .replace(/[<>:"|?*]/gu, '-')
        .replace(/^\.+/u, '')
        .replace(/[. ]+$/u, ''),
    )
    .filter((seg) => seg !== '')
    .map((seg) => (RESERVED.test(seg.split('.')[0].trimEnd()) ? `_${seg}` : seg))
  return segs.join('/') || 'Untitled.md'
}
