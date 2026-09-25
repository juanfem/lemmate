// Daily notes (SPEC §9): where a day's note lives in a vault. The TypeScript twin of
// `crates/core/src/daily.rs` — the settings live in the vault doc's `meta` map
// (`daily_folder`, `daily_format`, `daily_template`), the format is Obsidian's (Moment.js), and
// `corpus/daily-formats.json` holds both implementations to the same output.

export const DEFAULT_FOLDER = 'Daily'
export const DEFAULT_FORMAT = 'YYYY-MM-DD'
export const DEFAULT_TEMPLATE = 'Templates/Daily.md'

/** As stored; an empty field means the default. `folder: "/"` is the vault root. */
export interface DailySettings {
  folder: string
  format: string
  template: string
}

/** A calendar date; `month` is 1-based. */
export interface Day {
  year: number
  month: number
  day: number
}

export function folderOf(s: DailySettings): string {
  const f = s.folder.trim()
  if (f === '/') return ''
  return f.replace(/^\/+|\/+$/gu, '') || DEFAULT_FOLDER
}

export function formatOf(s: DailySettings): string {
  return s.format.trim() || DEFAULT_FORMAT
}

export function templateOf(s: DailySettings): string {
  return s.template.trim() || DEFAULT_TEMPLATE
}

export function pathFor(s: DailySettings, d: Day): string {
  const name = formatDate(formatOf(s), d)
  const folder = folderOf(s)
  return folder ? `${folder}/${name}.md` : `${name}.md`
}

export function today(now = new Date()): Day {
  return { year: now.getFullYear(), month: now.getMonth() + 1, day: now.getDate() }
}

export function iso(d: Day): string {
  return `${String(d.year).padStart(4, '0')}-${pad(d.month)}-${pad(d.day)}`
}

export function parseIso(s: string): Day | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/u.exec(s)
  if (!m) return null
  const d = { year: Number(m[1]), month: Number(m[2]), day: Number(m[3]) }
  return d.month >= 1 && d.month <= 12 && d.day >= 1 && d.day <= daysInMonth(d.year, d.month) ? d : null
}

export function daysInMonth(year: number, month: number): number {
  return new Date(Date.UTC(year, month, 0)).getUTCDate()
}

export function addDays(d: Day, n: number): Day {
  const t = new Date(Date.UTC(d.year, d.month - 1, d.day + n))
  return { year: t.getUTCFullYear(), month: t.getUTCMonth() + 1, day: t.getUTCDate() }
}

export function compare(a: Day, b: Day): number {
  return a.year - b.year || a.month - b.month || a.day - b.day
}

/** 0 = Sunday … 6 = Saturday. */
export function weekday(d: Day): number {
  return new Date(Date.UTC(d.year, d.month - 1, d.day)).getUTCDay()
}

function ordinal(d: Day): number {
  return (Date.UTC(d.year, d.month - 1, d.day) - Date.UTC(d.year, 0, 1)) / 86_400_000 + 1
}

function isoWeek(d: Day): [number, number] {
  const thursday = addDays(d, 3 - ((weekday(d) + 6) % 7))
  return [thursday.year, Math.floor((ordinal(thursday) - 1) / 7) + 1]
}

function localeWeek(d: Day): [number, number] {
  const saturday = addDays(d, 6 - weekday(d))
  return [saturday.year, Math.floor((ordinal(saturday) - 1) / 7) + 1]
}

function pad(n: number, width = 2): string {
  return String(n).padStart(width, '0')
}

export const MONTHS = [
  'January', 'February', 'March', 'April', 'May', 'June',
  'July', 'August', 'September', 'October', 'November', 'December',
]
export const WEEKDAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday']

/** Longest first, so `MMMM` wins over `MM` — the same list, in the same order, as the Rust. */
const TOKENS = [
  'YYYY', 'GGGG', 'gggg', 'MMMM', 'DDDD', 'dddd', 'MMM', 'DDD', 'ddd', 'YY', 'GG', 'gg', 'MM', 'DD',
  'Do', 'dd', 'WW', 'ww', 'Q', 'M', 'D', 'd', 'E', 'e', 'W', 'w',
]

function ordinalSuffix(n: number): string {
  if (n % 100 >= 11 && n % 100 <= 13) return 'th'
  return ['th', 'st', 'nd', 'rd'][n % 10] ?? 'th'
}

function token(t: string, d: Day): string {
  const month = MONTHS[d.month - 1] ?? ''
  const wd = WEEKDAYS[weekday(d)] ?? ''
  switch (t) {
    case 'YYYY': return pad(d.year, 4)
    case 'YY': return pad(((d.year % 100) + 100) % 100)
    case 'Q': return String(Math.floor((d.month - 1) / 3) + 1)
    case 'MMMM': return month
    case 'MMM': return month.slice(0, 3)
    case 'MM': return pad(d.month)
    case 'M': return String(d.month)
    case 'DDDD': return pad(ordinal(d), 3)
    case 'DDD': return String(ordinal(d))
    case 'DD': return pad(d.day)
    case 'D': return String(d.day)
    case 'Do': return `${d.day}${ordinalSuffix(d.day)}`
    case 'dddd': return wd
    case 'ddd': return wd.slice(0, 3)
    case 'dd': return wd.slice(0, 2)
    case 'd': case 'e': return String(weekday(d))
    case 'E': return String(((weekday(d) + 6) % 7) + 1)
    case 'GGGG': return pad(isoWeek(d)[0], 4)
    case 'GG': return pad(isoWeek(d)[0] % 100)
    case 'WW': return pad(isoWeek(d)[1])
    case 'W': return String(isoWeek(d)[1])
    case 'gggg': return pad(localeWeek(d)[0], 4)
    case 'gg': return pad(localeWeek(d)[0] % 100)
    case 'ww': return pad(localeWeek(d)[1])
    case 'w': return String(localeWeek(d)[1])
    default: return t
  }
}

/** Split a format into literal text and tokens; `[…]` escapes, an unterminated `[` is text. */
function lex(format: string): { tok: string | null; text: string }[] {
  const out: { tok: string | null; text: string }[] = []
  let rest = format
  while (rest.length > 0) {
    if (rest[0] === '[') {
      const end = rest.indexOf(']', 1)
      if (end > 0) {
        out.push({ tok: null, text: rest.slice(1, end) })
        rest = rest.slice(end + 1)
        continue
      }
    }
    const t = TOKENS.find((k) => rest.startsWith(k))
    if (t) {
      out.push({ tok: t, text: t })
      rest = rest.slice(t.length)
    } else {
      const c = String.fromCodePoint(rest.codePointAt(0) ?? 0)
      out.push({ tok: null, text: c })
      rest = rest.slice(c.length)
    }
  }
  return out
}

export function formatDate(format: string, d: Day): string {
  return lex(format).map((p) => (p.tok ? token(p.tok, d) : p.text)).join('')
}

const escape = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/gu, '\\$&')

/**
 * The day a vault path names under these settings, or null. Used to step between daily notes
 * and to mark the calendar, so it only has to read what `pathFor` writes: the pattern is built
 * from the format, and a candidate is accepted only if formatting it again gives the same path.
 */
export function dayOf(s: DailySettings, path: string): Day | null {
  const folder = folderOf(s)
  let name = path
  if (folder) {
    if (!path.startsWith(`${folder}/`)) return null
    name = path.slice(folder.length + 1)
  }
  if (!name.endsWith('.md')) return null
  name = name.slice(0, -3)
  let year: number | null = null
  let month: number | null = null
  let day: number | null = null
  const groups: ((v: string) => void)[] = []
  const num = (set: (n: number) => void) => (v: string) => set(Number.parseInt(v, 10))
  const names = (list: string[], len: number | null, set: (n: number) => void) => (v: string) =>
    set(list.findIndex((m) => (len ? m.slice(0, len) : m) === v) + 1)
  let re = ''
  for (const p of lex(formatOf(s))) {
    switch (p.tok) {
      case null: re += escape(p.text); break
      case 'YYYY': re += '(\\d{4})'; groups.push(num((n) => (year = n))); break
      case 'MM': re += '(\\d{2})'; groups.push(num((n) => (month = n))); break
      case 'M': re += '(\\d{1,2})'; groups.push(num((n) => (month = n))); break
      case 'MMMM': re += `(${MONTHS.join('|')})`; groups.push(names(MONTHS, null, (n) => (month = n))); break
      case 'MMM': re += `(${MONTHS.map((m) => m.slice(0, 3)).join('|')})`; groups.push(names(MONTHS, 3, (n) => (month = n))); break
      case 'DD': re += '(\\d{2})'; groups.push(num((n) => (day = n))); break
      case 'D': re += '(\\d{1,2})'; groups.push(num((n) => (day = n))); break
      case 'Do': re += '(\\d{1,2})(?:st|nd|rd|th)'; groups.push(num((n) => (day = n))); break
      case 'dddd': re += `(?:${WEEKDAYS.join('|')})`; break
      case 'ddd': re += `(?:${WEEKDAYS.map((w) => w.slice(0, 3)).join('|')})`; break
      case 'dd': re += `(?:${WEEKDAYS.map((w) => w.slice(0, 2)).join('|')})`; break
      // Anything else is derivable from the date (or not enough to find it): match and ignore.
      default: re += '\\d+'
    }
  }
  const m = new RegExp(`^${re}$`, 'u').exec(name)
  if (!m) return null
  groups.forEach((set, i) => set(m[i + 1] ?? ''))
  if (year === null || month === null || day === null) return null
  const d = parseIso(`${pad(year, 4)}-${pad(month)}-${pad(day)}`)
  return d && pathFor(s, d) === path ? d : null
}
