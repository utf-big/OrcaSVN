import { readonly, ref } from 'vue'
import { open } from '@tauri-apps/plugin-shell'
import packageInfo from '../../package.json'
import { isNewerVersion } from '@/utils/version'

/**
 * Update checking is disabled in this build.
 *
 * This application is a derivative of OrcaSVN, and the release feed below belongs
 * to the upstream project. Offering it here would point users at a download that
 * is not this app, so the feature is switched off rather than removed: the code
 * stays intact and can be enabled again by flipping this flag.
 */
export const AUTO_UPDATE_ENABLED = false

const LATEST_RELEASE_API = 'https://api.github.com/repos/wustites/OrcaSVN/releases/latest'
export const RELEASES_PAGE = 'https://github.com/wustites/OrcaSVN/releases'
const REQUEST_TIMEOUT_MS = 10_000

export type UpdateCheckState =
  | 'idle'
  | 'checking'
  | 'upToDate'
  | 'available'
  | 'failed'
  | 'disabled'

const state = ref<UpdateCheckState>(AUTO_UPDATE_ENABLED ? 'idle' : 'disabled')
const latestVersion = ref<string | null>(null)
const releaseUrl = ref(RELEASES_PAGE)
let pending: Promise<void> | null = null

async function fetchLatestRelease(): Promise<void> {
  state.value = 'checking'
  const controller = new AbortController()
  const timer = window.setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS)
  try {
    const response = await fetch(LATEST_RELEASE_API, {
      headers: { Accept: 'application/vnd.github+json' },
      signal: controller.signal,
    })
    if (!response.ok) throw new Error(`HTTP ${response.status}`)
    const release = await response.json() as { tag_name?: unknown; html_url?: unknown }
    const tag = typeof release.tag_name === 'string' ? release.tag_name : ''
    if (isNewerVersion(tag, packageInfo.version)) {
      latestVersion.value = tag.replace(/^v/, '')
      releaseUrl.value = typeof release.html_url === 'string' ? release.html_url : RELEASES_PAGE
      state.value = 'available'
    } else {
      state.value = 'upToDate'
    }
  } catch {
    // Offline or API errors stay silent; the settings page offers a manual retry.
    state.value = 'failed'
  } finally {
    window.clearTimeout(timer)
  }
}

export function useUpdateCheck() {
  const checkForUpdates = () => {
    // With the feature off nothing may reach the network, not even a manual check.
    if (!AUTO_UPDATE_ENABLED) return Promise.resolve()
    pending ??= fetchLatestRelease().finally(() => { pending = null })
    return pending
  }

  const openReleasePage = () => open(releaseUrl.value)

  return {
    currentVersion: packageInfo.version,
    state: readonly(state),
    latestVersion: readonly(latestVersion),
    checkForUpdates,
    openReleasePage,
  }
}
