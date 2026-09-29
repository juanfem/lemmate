// The note's measure: a centred column of `--measure` (46rem), or the whole width of the pane.
// A preference of the reader's screen rather than of any note, so it lives in this browser's
// storage and applies to every note, pane and window on the origin at once.

export const MEASURE_KEY = 'lemmate.measure'

/** Is the text set to fill the pane? Storage that holds nothing, or cannot be read, says no. */
export function readFullWidth(storage: Pick<Storage, 'getItem'> | undefined): boolean {
  try {
    return storage?.getItem(MEASURE_KEY) === 'full'
  } catch {
    return false
  }
}

export function writeFullWidth(storage: Pick<Storage, 'setItem' | 'removeItem'> | undefined, full: boolean): void {
  try {
    if (full) storage?.setItem(MEASURE_KEY, 'full')
    else storage?.removeItem(MEASURE_KEY)
  } catch {
    /* storage may be unavailable: the choice then lasts as long as the page */
  }
}

/** Put the choice where the stylesheets look for it: `data-measure` on the root element. */
export function applyFullWidth(root: HTMLElement | undefined, full: boolean): void {
  if (!root) return
  if (full) root.dataset.measure = 'full'
  else delete root.dataset.measure
}
