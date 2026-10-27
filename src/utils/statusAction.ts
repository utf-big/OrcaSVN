import type { SvnStatus } from '@/types'

/** Strategies accepted by `svn resolve --accept`, in the order the menu shows them. */
export const RESOLVE_STRATEGIES = ['working', 'mine-full', 'theirs-full'] as const
export type ResolveStrategy = typeof RESOLVE_STRATEGIES[number]

export type FileOperation = 'revert' | 'deleteUnversioned' | 'scheduleDelete' | 'resolve'

export function isConflicted(file: SvnStatus): boolean {
  return file.status_code === 'conflicted' || file.prop_status === 'conflicted'
}

/**
 * The single destructive action offered on the file row itself: an unversioned
 * target can only be deleted from disk, everything else reverts through SVN.
 */
export function rowOperation(file: SvnStatus): 'deleteUnversioned' | 'revert' {
  return file.status_code === 'unversioned' ? 'deleteUnversioned' : 'revert'
}

/**
 * A versioned file that vanished from disk is `missing`. `svn revert` restores
 * it and `svn delete` schedules its removal; both need to stay reachable.
 */
export function canScheduleDelete(file: SvnStatus): boolean {
  return file.status_code === 'missing'
}

/**
 * The file exists on disk, but its name differs from the repository spelling by
 * Unicode normalization (NFC vs NFD). SVN therefore reports the same file as
 * both unversioned and missing; only a rename can settle the working copy, so
 * revert and scheduled deletion must not be offered here.
 */
export function canRepairName(file: SvnStatus): boolean {
  return file.status_code === 'name_mismatch'
}

/**
 * The repository versions both Unicode spellings of one name. macOS can only
 * create one of them, so one entry is always missing and no local rename can
 * settle it: the duplicate has to be deleted from version control (and committed)
 * before a checkout can be clean.
 */
export function canDeleteDuplicate(file: SvnStatus): boolean {
  return file.status_code === 'name_duplicate'
}

/** Conflicted files are excluded from commit until SVN records a resolution. */
export function canResolve(file: SvnStatus): boolean {
  return isConflicted(file)
}

export function isResolveStrategy(value: unknown): value is ResolveStrategy {
  return typeof value === 'string' && (RESOLVE_STRATEGIES as readonly string[]).includes(value)
}
