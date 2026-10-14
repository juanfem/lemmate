// The spell-check preference as a rune, for the command that names the other state and the
// editors that follow it. `lib/spellcheck.ts` holds the storage; this keeps it and the UI in
// step, across windows too — another window's choice arrives as a `storage` event.
import { SPELLCHECK_KEY, readSpellcheck, writeSpellcheck } from './spellcheck.ts'

const storage = typeof localStorage === 'undefined' ? undefined : localStorage

let on = $state(readSpellcheck(storage))

if (typeof window !== 'undefined') {
  window.addEventListener('storage', (e) => {
    if (e.key !== SPELLCHECK_KEY && e.key !== null) return
    on = readSpellcheck(storage)
  })
}

export const spelling = {
  get on() {
    return on
  },
  toggle() {
    on = !on
    writeSpellcheck(storage, on)
  },
}
