import i18n from '@/i18n'

/**
 * Error payload returned by the Rust commands. `code` is stable and drives the
 * explanation, `message` keeps the original SVN context.
 */
export interface CommandErrorBody {
  code: string
  message: string
  detail?: string | null
}

/** Codes the interface can explain with an actionable hint. */
const HINT_KEYS: Record<string, string> = {
  svn_locked: 'error.hint.svnLocked',
  svn_auth: 'error.hint.svnAuth',
  svn_conflict: 'error.hint.svnConflict',
  svn_network: 'error.hint.svnNetwork',
  svn_timeout: 'error.hint.svnTimeout',
  svn_not_found: 'error.hint.svnNotFound',
  svn_path_missing: 'error.hint.svnPathMissing',
  svn_invalid_arguments: 'error.hint.svnInvalidArguments',
  workspace_invalid: 'error.hint.workspaceInvalid',
  path_outside_workspace: 'error.hint.pathOutsideWorkspace',
  stash_too_large: 'error.hint.stashTooLarge',
  target_exists: 'error.hint.targetExists',
}

export function parseCommandError(error: unknown): CommandErrorBody {
  if (error && typeof error === 'object') {
    const candidate = error as Partial<CommandErrorBody>
    if (typeof candidate.message === 'string' && candidate.message.length > 0) {
      return {
        code: typeof candidate.code === 'string' && candidate.code ? candidate.code : 'unknown',
        message: candidate.message,
        detail: typeof candidate.detail === 'string' ? candidate.detail : null,
      }
    }
  }
  if (typeof error === 'string' && error.trim().length > 0) {
    return { code: 'unknown', message: error, detail: null }
  }
  if (error instanceof Error && error.message) {
    return { code: 'unknown', message: error.message, detail: null }
  }
  return { code: 'unknown', message: String(error), detail: null }
}

export function errorCode(error: unknown): string {
  return parseCommandError(error).code
}

export function hasErrorCode(error: unknown, code: string): boolean {
  return errorCode(error) === code
}

/** i18n key for the suggested remedy, or null when the code needs no advice. */
export function errorHintKey(error: unknown): string | null {
  return HINT_KEYS[errorCode(error)] ?? null
}

/**
 * Human-readable error: the original SVN context plus a localized suggestion.
 * Every view reports command failures through this helper so a `locked` or
 * `authorization` failure tells the user what to do next.
 */
export function errorMessage(error: unknown): string {
  const { message } = parseCommandError(error)
  const hintKey = errorHintKey(error)
  if (!hintKey) return message
  const hint = i18n.global.t(hintKey)
  return hint && hint !== hintKey ? `${message}\n${hint}` : message
}
