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
