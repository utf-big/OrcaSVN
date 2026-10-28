import { invoke } from '@tauri-apps/api/core'
import type { CommandResult, SvnStatus, SvnLogEntry, SvnInfo, SvnAuthUser, DiffResult, BlameLine, StashEntry, StashUnversionedEntry } from '@/types'

export async function configureSvnExecutable(executable?: string): Promise<string> {
  return invoke<string>('configure_svn_executable', {
    executable: executable?.trim() || null,
  })
}

/**
 * True once the checkout has created the working copy (`.svn` exists). SVN
 * creates it before transferring files, so this is true very early.
 */
export async function isWorkingCopy(path: string): Promise<boolean> {
  return invoke<boolean>('svn_path_is_working_copy', { path })
}

export interface CheckoutReport {
  output: string
  attempts: number
  revision: number | null
  salvaged: boolean
  failed: string[]
  missingFiles: string[]
}

export interface SalvageReport {
  failed: string[]
  missingFiles: string[]
  output: string
}

/**
 * Continues a cancelled checkout of an existing working copy. Uses an update at
 * full depth, so a partial copy (even one left at depth immediates) is completed.
 */
export async function svnResumeCheckout(path: string): Promise<CheckoutReport> {
  return invoke<CheckoutReport>('svn_resume_checkout', { path })
}

/**
 * Fetches everything the server can still serve after a storage fault, skipping
 * the unreadable subtrees and reporting them.
 */
export async function svnCheckoutSalvage(url: string, path: string): Promise<SalvageReport> {
  return invoke<SalvageReport>('svn_checkout_salvage', { url, path })
}

/**
 * Streams progress through the `checkout-progress` event while the checkout
 * runs, and resolves with the summary once it finishes.
 */
export async function svnCheckoutStream(
  url: string,
  path: string,
  revision?: number
): Promise<CheckoutReport> {
  return invoke<CheckoutReport>('svn_checkout_stream', { url, path, revision })
}

export async function svnUpdate(path: string, revision?: number): Promise<CommandResult> {
  return invoke<CommandResult>('svn_update', { path, revision })
}

/** Renames files whose on-disk NFC/NFD form differs from the repository spelling. */
export async function svnRepairNames(path: string, files: string[]): Promise<number> {
  return invoke<number>('svn_repair_names', { path, files })
}

/** Deletes a repository-side duplicate spelling, keeping the local file. */
export async function svnDeleteDuplicates(path: string, files: string[]): Promise<CommandResult> {
  return invoke<CommandResult>('svn_delete_duplicates', { path, files })
}

/** Aborts the running checkout or update for this working copy. */
export async function svnCancel(path: string): Promise<boolean> {
  return invoke<boolean>('svn_cancel', { path })
}

export async function svnCommit(
  path: string,
  message: string,
  files?: string[]
): Promise<CommandResult> {
  return invoke<CommandResult>('svn_commit', { path, message, files })
}

export async function svnStatus(path: string, force = false): Promise<SvnStatus[]> {
  return invoke<SvnStatus[]>('svn_status', { path, force })
}

export async function svnLog(
  path: string,
  limit?: number,
  startRev?: number,
  endRev?: number,
  keyword?: string,
  author?: string,
  dateFrom?: string,
  dateTo?: string
): Promise<SvnLogEntry[]> {
  return invoke<SvnLogEntry[]>('svn_log', { path, limit, startRev, endRev, keyword, author, dateFrom, dateTo })
}

export async function svnCurrentUser(path: string): Promise<SvnAuthUser | null> {
  return invoke<SvnAuthUser | null>('svn_current_user', { path })
}

export async function svnInfo(path: string): Promise<SvnInfo> {
  return invoke<SvnInfo>('svn_info', { path })
}

export async function svnLocalRevision(path: string): Promise<number> {
  return invoke<number>('svn_local_revision', { path })
}

export async function svnRemoteInfo(path: string): Promise<SvnInfo> {
  return invoke<SvnInfo>('svn_remote_info', { path })
}

export async function svnDiff(
  workspacePath: string,
  file: string,
  oldRev?: number,
  newRev?: number
): Promise<DiffResult> {
  return invoke<DiffResult>('svn_diff', { workspacePath, file, oldRev, newRev })
}

export async function svnApplyPatch(
  workspacePath: string,
  patch: string,
  reverse = false
): Promise<CommandResult> {
  return invoke<CommandResult>('svn_apply_patch', { workspacePath, patch, reverse })
}

export async function svnBlame(workspacePath: string, file: string): Promise<BlameLine[]> {
  return invoke<BlameLine[]>('svn_blame', { workspacePath, file })
}

export async function svnAdd(path: string, files: string[]): Promise<CommandResult> {
  return invoke<CommandResult>('svn_add', { path, files })
}

export async function svnDelete(path: string, files: string[]): Promise<CommandResult> {
  return invoke<CommandResult>('svn_delete', { path, files })
}

export async function svnRevert(path: string, files: string[]): Promise<CommandResult> {
  return invoke<CommandResult>('svn_revert', { path, files })
}

export async function deleteUnversioned(path: string, files: string[]): Promise<CommandResult> {
  return invoke<CommandResult>('delete_unversioned', { path, files })
}

export async function readUnversionedFiles(path: string, files: string[]): Promise<StashUnversionedEntry[]> {
  return invoke<StashUnversionedEntry[]>('read_unversioned_files', { path, files })
}

export async function restoreUnversionedFiles(
  path: string,
  files: StashUnversionedEntry[],
  dryRun = false
): Promise<void> {
  return invoke<void>('restore_unversioned_files', { path, files, dryRun })
}

export async function svnResolve(
  path: string,
  files: string[],
  strategy: string
): Promise<CommandResult> {
  return invoke<CommandResult>('svn_resolve', { path, files, strategy })
}

export async function svnCleanup(path: string): Promise<CommandResult> {
  return invoke<CommandResult>('svn_cleanup', { path })
}

export async function svnSwitch(path: string, url: string): Promise<CommandResult> {
  return invoke<CommandResult>('svn_switch', { path, url })
}

export interface GitignoreData {
  content: string
  mtime: number
}

export async function readGitignore(path: string): Promise<GitignoreData | null> {
  return invoke<GitignoreData | null>('read_gitignore', { path })
}

export async function svnMerge(
  path: string,
  source: string,
  revStart: number,
  revEnd: number
): Promise<CommandResult> {
  return invoke<CommandResult>('svn_merge', { path, source, revStart, revEnd })
}

export type OpenWorkspaceTarget = 'explorer' | 'vscode' | 'terminal'

export async function openWorkspaceTarget(
  path: string,
  target: OpenWorkspaceTarget
): Promise<void> {
  return invoke<void>('open_workspace_target', { path, target })
}

export async function revealWorkspaceFile(path: string, file: string): Promise<void> {
  return invoke<void>('reveal_workspace_file', { path, file })
}

export async function launchWorkspacePath(): Promise<string | null> {
  return invoke<string | null>('launch_workspace_path')
}

export async function saveStash(stash: StashEntry): Promise<void> {
  return invoke<void>('save_stash', { stash })
}

export async function listStashes(workspacePath: string): Promise<StashEntry[]> {
  return invoke<StashEntry[]>('list_stashes', { workspacePath })
}

export async function deleteStash(workspacePath: string, id: string): Promise<void> {
  return invoke<void>('delete_stash', { workspacePath, id })
}

/** Moves stash records written by older versions out of local storage. */
export async function importLegacyStashes(workspacePath: string, stashes: StashEntry[]): Promise<number> {
  return invoke<number>('import_legacy_stashes', { workspacePath, stashes })
}
