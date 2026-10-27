/** Which stream a checkout line came from. */
export type CheckoutLineKind = 'out' | 'err' | 'status'

export interface CheckoutLine {
  kind: CheckoutLineKind
  text: string
}

/** SVN marks each processed item with one letter, e.g. `A    path/to/file`. */
export interface CheckoutCounters {
  added: number
  updated: number
  deleted: number
  conflicted: number
  restored: number
  skipped: number
}

/** Only the tail is rendered; a full checkout can print tens of thousands of lines. */
export const VISIBLE_LINE_LIMIT = 300

export function emptyCounters(): CheckoutCounters {
  return { added: 0, updated: 0, deleted: 0, conflicted: 0, restored: 0, skipped: 0 }
}

const LINE_MARKERS: Record<string, keyof CheckoutCounters> = {
  A: 'added',
  U: 'updated',
  G: 'updated',
  D: 'deleted',
  C: 'conflicted',
  R: 'restored',
  E: 'restored',
  S: 'skipped',
}

/**
 * Classifies one line of `svn checkout` output. Returns null for prose such as
 * `Checked out revision 123.` or for diagnostics, which are counted separately.
 */
export function classifyCheckoutLine(text: string): keyof CheckoutCounters | null {
  const match = /^([A-Z]) {1,5}\S/.exec(text)
  if (!match) return null
  return LINE_MARKERS[match[1]] ?? null
}

export interface CheckoutProgressState {
  lines: CheckoutLine[]
  total: number
  counters: CheckoutCounters
}

export function createProgressState(): CheckoutProgressState {
  return { lines: [], total: 0, counters: emptyCounters() }
}

/**
 * Appends one line, keeps the visible window bounded and updates the counters.
 * Mutates in place so a reactive object can be passed straight from a view.
 */
export function pushCheckoutLine(
  state: CheckoutProgressState,
  kind: CheckoutLineKind,
  text: string,
  limit = VISIBLE_LINE_LIMIT,
): void {
  state.total += 1
  const bucket = classifyCheckoutLine(text)
  if (bucket) state.counters[bucket] += 1
  state.lines.push({ kind, text })
  if (state.lines.length > limit) state.lines.splice(0, state.lines.length - limit)
}

/** Sum of every counted item, used as a coarse progress indicator. */
export function countedItems(counters: CheckoutCounters): number {
  return Object.values(counters).reduce((total, value) => total + value, 0)
}
