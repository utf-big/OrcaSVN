import type { SvnInfo } from '@/types'

/** `svn merge -r START:END` needs an inclusive, ascending revision range. */
export function isValidRevisionRange(start: number, end: number): boolean {
  return Number.isInteger(start) && Number.isInteger(end) && start >= 1 && end >= start
}

/** Local revision of the working copy, or 1 when it is not known yet. */
export function defaultMergeStart(info: SvnInfo | null): number {
  return info && info.revision >= 1 ? info.revision : 1
}
