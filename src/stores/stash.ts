import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import type { StashEntry } from '@/types'
import { deleteStash, importLegacyStashes, listStashes, saveStash } from '@/api/svn'

/**
 * Older versions kept stashes in WebView local storage, where clearing site
 * data destroyed them. Records now live in the app data directory; this key is
 * only read once to migrate what is still there.
 */
const LEGACY_KEY = 'orcasvn-stashes'

function isStashEntry(value: unknown): value is StashEntry {
  if (!value || typeof value !== 'object') return false
  const candidate = value as Partial<StashEntry>
  return typeof candidate.id === 'string'
    && typeof candidate.name === 'string'
    && typeof candidate.workspacePath === 'string'
    && typeof candidate.createdAt === 'string'
    && typeof candidate.patch === 'string'
    && Array.isArray(candidate.files)
    && typeof candidate.hunkCount === 'number'
}

/**
 * Splits legacy records for one workspace from the rest. Exported so the
 * migration rule is testable without local storage or the backend.
 */
export function splitLegacyEntries(raw: unknown, workspacePath: string): {
  imported: StashEntry[]
  remaining: unknown[]
} {
  const all = Array.isArray(raw) ? raw : []
  const imported: StashEntry[] = []
  const remaining: unknown[] = []
  for (const entry of all) {
    // Invalid records are dropped rather than written back, so a corrupt entry
    // cannot survive every migration attempt.
    if (!isStashEntry(entry)) continue
    if (entry.workspacePath === workspacePath) imported.push(entry)
    else remaining.push(entry)
  }
  return { imported, remaining }
}

function takeLegacyEntries(workspacePath: string): StashEntry[] {
  let raw: unknown
  try {
    raw = JSON.parse(localStorage.getItem(LEGACY_KEY) || '[]')
  } catch {
    // Malformed legacy data must not block the workspace.
    try {
      localStorage.removeItem(LEGACY_KEY)
    } catch {
      // Nothing to clean up when storage is unavailable.
    }
    return []
  }
  const { imported, remaining } = splitLegacyEntries(raw, workspacePath)
  if (imported.length === 0) return []
  // Keep the not-yet-imported workspaces available for their own first load.
  try {
    if (remaining.length === 0) localStorage.removeItem(LEGACY_KEY)
    else localStorage.setItem(LEGACY_KEY, JSON.stringify(remaining))
  } catch {
    // Storage is optional; the import below still runs.
  }
  return imported
}

export const useStashStore = defineStore('stash', () => {
  const entries = ref<StashEntry[]>([])
  const loadedWorkspaces = ref<string[]>([])
  const loading = new Map<string, Promise<void>>()

  const currentEntries = computed(() => (workspacePath: string | null) => {
    if (!workspacePath) return []
    return entries.value.filter(entry => entry.workspacePath === workspacePath)
  })

  const mergeWorkspaceEntries = (workspacePath: string, records: StashEntry[]) => {
    entries.value = [
      ...records,
      ...entries.value.filter(entry => entry.workspacePath !== workspacePath),
    ]
  }

  /** Loads the records of one working copy once per session. */
  async function ensureLoaded(workspacePath: string | null): Promise<void> {
    if (!workspacePath || loadedWorkspaces.value.includes(workspacePath)) return
    const inFlight = loading.get(workspacePath)
    if (inFlight) return inFlight

    const task = (async () => {
      try {
        const legacy = takeLegacyEntries(workspacePath)
        if (legacy.length > 0) {
          const imported = await importLegacyStashes(workspacePath, legacy).catch(() => 0)
          if (imported > 0) {
            mergeWorkspaceEntries(workspacePath, legacy)
            loadedWorkspaces.value = [...loadedWorkspaces.value, workspacePath]
            return
          }
        }
        const records = await listStashes(workspacePath)
        mergeWorkspaceEntries(workspacePath, records)
        loadedWorkspaces.value = [...loadedWorkspaces.value, workspacePath]
      } catch (error) {
        // Leave the workspace unloaded so the next visit retries, and never let
        // a rejected promise escape from a fire-and-forget watcher.
        console.warn('Failed to load stash records', error)
      }
    })()

    loading.set(workspacePath, task)
    try {
      await task
    } finally {
      loading.delete(workspacePath)
    }
  }

  async function addEntry(entry: StashEntry): Promise<void> {
    // Persist before publishing the new state: a failed write must not make the
    // UI claim that the only copy of a patch was saved.
    await saveStash(entry)
    entries.value = [entry, ...entries.value.filter(item => item.id !== entry.id)]
  }

  async function removeEntry(id: string): Promise<void> {
    const entry = entries.value.find(item => item.id === id)
    if (!entry) return
    await deleteStash(entry.workspacePath, id)
    entries.value = entries.value.filter(item => item.id !== id)
  }

  return { entries, currentEntries, ensureLoaded, addEntry, removeEntry }
})
