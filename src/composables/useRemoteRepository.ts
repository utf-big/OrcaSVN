import { computed } from 'vue'
import { open } from '@tauri-apps/plugin-shell'
import { useWorkspaceStore } from '@/stores/workspace'

/**
 * The remote address of the current working copy, and the action that opens it.
 *
 * SVN reports repository URLs (`http://`, `svn://`, `file://`), so the address is
 * handed to the system handler rather than rendered in-app.
 */
export function useRemoteRepository() {
  const workspaceStore = useWorkspaceStore()

  /** Empty until `svn info` has described the current working copy. */
  const remoteRepositoryUrl = computed(() => workspaceStore.svnInfo?.url ?? '')

  /** Returns false when there is nothing to open; failures propagate to the caller. */
  const openRemoteRepository = async () => {
    const url = remoteRepositoryUrl.value
    if (!url) return false
    await open(url)
    return true
  }

  return { remoteRepositoryUrl, openRemoteRepository }
}
