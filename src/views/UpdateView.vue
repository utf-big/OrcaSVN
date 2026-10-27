<template>
  <div class="update-view">
    <el-card class="update-card">
      <template #header>
        <div class="card-header">
          <span class="card-title">
            <el-icon><RefreshRight /></el-icon>
            {{ $t('update.title') }}
          </span>
          <div class="header-actions">
            <el-button size="small" @click="loadUpdateState(true)" :loading="loading">
              <el-icon><Refresh /></el-icon>
              {{ $t('update.check') }}
            </el-button>
            <el-button size="small" :loading="cleaning"
              :title="$t('workspace.quickCleanupHint')"
              :disabled="!workspaceStore.currentPath || updating"
              @click="doCleanup">
              <el-icon><Brush /></el-icon>
              {{ $t('common.cleanup') }}
            </el-button>
            <el-button type="primary" size="small" @click="doUpdate" :loading="updating"
              :disabled="!workspaceStore.currentPath || checkoutRunning">
              <el-icon><RefreshRight /></el-icon>
              {{ $t('common.update') }}
            </el-button>
            <el-button v-if="updating" type="danger" size="small" plain @click="cancelUpdate">
              <el-icon><Close /></el-icon>
              {{ $t('common.cancel') }}
            </el-button>
          </div>
        </div>
      </template>

      <el-empty v-if="!workspaceStore.currentPath" :description="$t('log.openWorkspaceFirst')" class="empty-workspace" />

      <div v-else class="update-content">
        <section v-if="resumeVisible" class="resume-panel">
          <div class="resume-header">
            <span class="resume-title">
              <el-icon><Download /></el-icon>
              {{ $t('checkout.resumeTitle') }}
            </span>
            <el-tag size="small" :type="resumePhase === 'running' ? 'primary' : 'warning'">{{ resumeStatusText }}</el-tag>
          </div>
          <p class="resume-path"><code>{{ resumePath }}</code></p>
          <p class="resume-hint">
            {{ resumePhase === 'running' ? $t('checkout.runningElsewhere') : resumePhase === 'cancelled' ? $t('checkout.resumeCancelled') : $t('checkout.resumeFailed') }}
          </p>
          <div v-if="countedItems(resumeProgress.counters) > 0 || resumeProgress.total > 0" class="resume-counters">
            {{ $t('checkout.progressCounters', resumeProgress.counters) }} · {{ $t('checkout.progressLines', { count: resumeProgress.total, shown: resumeProgress.lines.length }) }}
          </div>
          <div v-if="resumeProgress.lines.length" ref="resumeLog" class="resume-log" role="log">
            <div v-for="(line, index) in resumeProgress.lines" :key="index" class="progress-line" :class="`is-${line.kind}`">{{ line.text }}</div>
          </div>
          <div class="resume-actions">
            <el-button v-if="resumePhase !== 'running'" type="primary" size="small" :loading="resumeRunning" @click="resumeCheckout">
              <el-icon><RefreshRight /></el-icon>
              {{ $t('checkout.resumeAction') }}
            </el-button>
            <el-button v-if="resumePhase === 'running'" type="danger" size="small" plain @click="cancelResume">
              <el-icon><Close /></el-icon>
              {{ $t('common.cancel') }}
            </el-button>
            <el-button v-if="resumePhase !== 'running'" size="small" @click="discardResume">
              {{ $t('checkout.discard') }}
            </el-button>
          </div>
        </section>

        <div class="revision-strip">
          <div>
            <span>{{ $t('update.localRevision') }}</span>
            <strong>r{{ localRevision || '-' }}</strong>
          </div>
          <div>
            <span>{{ $t('update.remoteRevision') }}</span>
            <strong>r{{ remoteRevision || '-' }}</strong>
          </div>
          <div>
            <span>{{ $t('update.incomingCount') }}</span>
            <strong>{{ incomingLogs.length }}</strong>
          </div>
          <div>
            <span>{{ $t('update.localChangeCount') }}</span>
            <strong>{{ localChanges.length }}</strong>
          </div>
        </div>

        <section class="update-section">
          <div class="section-header">
            <h3>{{ $t('update.incomingTitle') }}</h3>
            <span>{{ $t('update.incomingHint') }}</span>
          </div>
          <el-table :data="incomingLogs" stripe class="update-table" max-height="280" row-key="revision">
            <el-table-column prop="revision" :label="$t('log.revision')" width="90" align="center">
              <template #default="{ row }">
                <el-tag type="primary" size="small" effect="plain">r{{ row.revision }}</el-tag>
              </template>
            </el-table-column>
            <el-table-column prop="author" :label="$t('log.author')" width="140" />
            <el-table-column :label="$t('log.date')" width="180">
              <template #default="{ row }">{{ formatDate(row.date) }}</template>
            </el-table-column>
            <el-table-column prop="message" :label="$t('log.commitMessage')" show-overflow-tooltip />
            <el-table-column :label="$t('log.changedFiles')" width="110" align="center">
              <template #default="{ row }">{{ row.changed_paths?.length || 0 }}</template>
            </el-table-column>
          </el-table>
          <div v-if="!loading && incomingLogs.length === 0" class="empty-line">
            {{ $t('update.noIncoming') }}
          </div>
        </section>

        <div class="list-divider"></div>

        <section class="update-section">
          <div class="section-header">
            <h3>{{ $t('update.localTitle') }}</h3>
            <span>{{ $t('update.localHint') }}</span>
          </div>
          <el-table :data="localChanges" stripe class="update-table" max-height="280" row-key="path">
            <el-table-column prop="status_code" :label="$t('commit.status')" width="120" align="center">
              <template #default="{ row }">
                <span class="status-badge" :class="getStatusClass(row.status_code)">
                  {{ $t(getStatusLabelKey(row.status_code)) }}
                </span>
              </template>
            </el-table-column>
            <el-table-column prop="path" :label="$t('commit.file')" show-overflow-tooltip>
              <template #default="{ row }">
                <button class="file-link" type="button" @click="viewDiff(row.path)">
                  {{ row.path }}
                </button>
              </template>
            </el-table-column>
            <el-table-column :label="$t('common.action')" width="90" align="center">
              <template #default="{ row }">
                <el-button text size="small" @click="viewDiff(row.path)">
                  {{ $t('common.diff') }}
                </el-button>
              </template>
            </el-table-column>
          </el-table>
          <div v-if="!loading && localChanges.length === 0" class="empty-line">
            {{ $t('workspace.noChanges') }}
          </div>
        </section>
      </div>
    </el-card>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onActivated, onDeactivated, reactive, onUnmounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus/es/components/message/index'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { svnCancel, svnCleanup, svnLog, svnRemoteInfo, svnResumeCheckout, svnUpdate } from '@/api/svn'
import { useWorkspaceStore } from '@/stores/workspace'
import { useCheckoutStore } from '@/stores/checkout'
import { useWorkspace } from '@/composables/useWorkspace'
import { getStatusClass, getStatusLabelKey } from '@/composables/useSvnStatus'
import { useSettings } from '@/composables/useSettings'
import type { SvnLogEntry } from '@/types'
import { errorMessage } from '@/utils/errors'
import {
  countedItems,
  createProgressState,
  pushCheckoutLine,
  type CheckoutLineKind,
} from '@/utils/checkoutProgress'

type UpdateStateCache = {
  remoteRevision: number
  incomingLogs: SvnLogEntry[]
}

const updateStateCache = new Map<string, UpdateStateCache>()

const router = useRouter()
const { t, locale } = useI18n()
const workspaceStore = useWorkspaceStore()
const checkoutStore = useCheckoutStore()
const { refreshStatusAfterMutation, refreshStatusIfStale } = useWorkspace()
const { settings } = useSettings()

const loading = ref(false)
const updating = ref(false)
const cleaning = ref(false)
// A running checkout owns the working copy; updating it would collide.
const checkoutRunning = computed(() => checkoutStore.running(workspaceStore.currentPath))
const cancelled = ref(false)
const incomingLogs = ref<SvnLogEntry[]>([])
const remoteRevision = ref(0)
let isActive = false
let updateStateTimer: number | undefined
let updateRequestGeneration = 0
const updateStateIntervalMs = 30_000

const localRevision = computed(() => workspaceStore.svnInfo?.revision || 0)
const localChanges = computed(() => workspaceStore.statusList.filter(status => status.status_code !== 'normal' && status.status_code !== ''))

const formatDate = (date: string) => date ? new Date(date).toLocaleString(locale.value) : ''

const applyCachedState = (path: string) => {
  const cached = updateStateCache.get(path)
  if (!cached) return false

  remoteRevision.value = cached.remoteRevision
  incomingLogs.value = cached.incomingLogs.map(entry => ({
    ...entry,
    changed_paths: [...(entry.changed_paths || [])],
  }))
  return true
}

const loadUpdateState = async (force = false) => {
  // A completed update must refresh shared workspace state even after navigation.
  if (!workspaceStore.currentPath || (!force && (!isActive || loading.value || updating.value))) return
  const requestedPath = workspaceStore.currentPath
  const generation = ++updateRequestGeneration
  const isCurrent = () => generation === updateRequestGeneration
    && workspaceStore.currentPath === requestedPath
    && isActive
  loading.value = true
  try {
    const refreshed = await (force ? refreshStatusAfterMutation() : refreshStatusIfStale(updateStateIntervalMs))
    if (!isCurrent() || !refreshed) return
    const info = await svnRemoteInfo(requestedPath)
    if (!isCurrent()) return
    remoteRevision.value = info.revision
    const local = workspaceStore.svnInfo?.revision || 0
    if (info.revision > local) {
      const logs = await svnLog(requestedPath, settings.logLimit || 50, info.revision, local + 1)
      if (!isCurrent()) return
      incomingLogs.value = logs
    } else {
      incomingLogs.value = []
    }
    updateStateCache.set(requestedPath, {
      remoteRevision: remoteRevision.value,
      incomingLogs: incomingLogs.value.map(entry => ({
        ...entry,
        changed_paths: [...(entry.changed_paths || [])],
      })),
    })
  } catch (err) {
    if (!isCurrent()) return
    workspaceStore.setError(errorMessage(err))
    ElMessage.error(`${t('common.error')}：${errorMessage(err)}`)
  } finally {
    if (generation === updateRequestGeneration) loading.value = false
  }
}

const doUpdate = async () => {
  if (!workspaceStore.currentPath) return
  const requestedPath = workspaceStore.currentPath
  cancelled.value = false
  updating.value = true
  try {
    await svnUpdate(requestedPath)
    if (workspaceStore.currentPath !== requestedPath) return
    updateStateCache.delete(requestedPath)
    await loadUpdateState(true)
    if (workspaceStore.currentPath !== requestedPath) return
    ElMessage.success(`${t('common.update')} ${t('common.success')}`)
  } catch (err) {
    if (workspaceStore.currentPath !== requestedPath) return
    // An aborted update can leave the working copy half merged: refresh the
    // shared status so the workspace shows what actually changed.
    if (cancelled.value) {
      ElMessage.info(t('update.cancelled'))
      await refreshStatusAfterMutation()
      return
    }
    workspaceStore.setError(errorMessage(err))
    ElMessage.error(`${t('common.error')}：${errorMessage(err)}`)
  } finally {
    updating.value = false
    cancelled.value = false
  }
}

/**
 * Clears a lock left behind by an interrupted SVN command. The same recovery is
 * also automatic on the next update, but the button makes it discoverable.
 */
const doCleanup = async () => {
  const path = workspaceStore.currentPath
  if (!path || cleaning.value) return
  cleaning.value = true
  try {
    await svnCleanup(path)
    ElMessage.success(`${t('common.cleanup')} ${t('common.success')}`)
    await refreshStatusAfterMutation()
  } catch (err) {
    workspaceStore.setError(String(err))
    ElMessage.error(`${t('common.error')}：${errorMessage(err)}`)
  } finally {
    cleaning.value = false
  }
}

const cancelUpdate = async () => {
  const path = workspaceStore.currentPath
  if (!path) return
  cancelled.value = true
  await svnCancel(path).catch(() => false)
}

const viewDiff = (path: string) => {
  router.push({ name: 'diff', query: { path } })
}

// A checkout started on the checkout page can be cancelled there and resumed
// here; the session survives the page switch because it lives in a store.
const resumeProgress = reactive(createProgressState())
const resumeRunning = ref(false)
const resumeCancelled = ref(false)
const resumeLog = ref<HTMLElement | null>(null)
let resumeUnlisten: UnlistenFn | null = null

const resumeSession = computed(() => {
  const session = checkoutStore.session
  if (!session || workspaceStore.currentPath !== session.path) return null
  return session
})
const resumeVisible = computed(() => Boolean(resumeSession.value) && resumeSession.value?.phase !== 'completed')
const resumePhase = computed(() => resumeSession.value?.phase ?? 'completed')
const resumePath = computed(() => resumeSession.value?.path ?? '')
const resumeStatusText = computed(() => {
  if (resumePhase.value === 'running') return t('checkout.statusRunning')
  if (resumePhase.value === 'cancelled') return t('checkout.statusCancelled')
  return t('checkout.statusFailed')
})

const stopResumeListening = () => {
  resumeUnlisten?.()
  resumeUnlisten = null
}

const scrollResumeLog = async () => {
  await nextTick()
  if (resumeLog.value) resumeLog.value.scrollTop = resumeLog.value.scrollHeight
}

const resumeCheckout = async () => {
  const session = resumeSession.value
  if (!session || resumeRunning.value) return
  resumeCancelled.value = false
  Object.assign(resumeProgress, createProgressState())
  resumeRunning.value = true
  checkoutStore.begin(session.url, session.path, session.revision)
  stopResumeListening()
  resumeUnlisten = await listen<{ kind: CheckoutLineKind; line: string }>('checkout-progress', event => {
    pushCheckoutLine(resumeProgress, event.payload.kind, event.payload.line)
    void scrollResumeLog()
  })
  try {
    const report = await svnResumeCheckout(session.path)
    checkoutStore.complete()
    if (report.missingFiles?.length) {
      ElMessage.warning(t('checkout.missingFiles', { count: report.missingFiles.length }))
    }
    await refreshStatusAfterMutation()
    ElMessage.success(t('checkout.statusSuccess'))
  } catch (err) {
    if (resumeCancelled.value) {
      checkoutStore.markCancelled()
      ElMessage.info(t('checkout.statusCancelled'))
    } else {
      checkoutStore.markFailed()
      ElMessage.error(errorMessage(err))
    }
  } finally {
    resumeRunning.value = false
    stopResumeListening()
  }
}

const cancelResume = async () => {
  const session = resumeSession.value
  if (!session) return
  resumeCancelled.value = true
  await svnCancel(session.path).catch(() => false)
}

const discardResume = () => {
  checkoutStore.clear()
  Object.assign(resumeProgress, createProgressState())
}

const stopUpdatePolling = () => {
  isActive = false
  stopResumeListening()
  if (updateStateTimer !== undefined) {
    window.clearInterval(updateStateTimer)
    updateStateTimer = undefined
  }
  updateRequestGeneration += 1
  loading.value = false
}

onActivated(() => {
  isActive = true
  if (workspaceStore.currentPath) {
    applyCachedState(workspaceStore.currentPath)
    void loadUpdateState()
  }
  updateStateTimer = window.setInterval(() => { void loadUpdateState() }, updateStateIntervalMs)
})

onDeactivated(stopUpdatePolling)
onUnmounted(() => {
  stopResumeListening()
  stopUpdatePolling()
})

watch(
  () => workspaceStore.currentPath,
  (path) => {
    updateRequestGeneration += 1
    loading.value = false
    incomingLogs.value = []
    remoteRevision.value = 0
    if (isActive && path && !applyCachedState(path)) {
      loadUpdateState()
    }
  }
)
</script>

<style scoped>
.update-view {
  height: 100%;
}

.update-card {
  min-height: 100%;
  border-radius: var(--app-radius-lg);
}

.card-header,
.header-actions {
  display: flex;
  align-items: center;
}

.card-header {
  justify-content: space-between;
}

.card-title {
  display: inline-flex;
  align-items: center;
  gap: var(--app-spacing-sm);
  font-weight: 700;
}

.header-actions {
  gap: var(--app-spacing-sm);
}

.empty-workspace {
  height: 420px;
}

.update-content {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.revision-strip {
  display: grid;
  grid-template-columns: repeat(4, minmax(120px, 1fr));
  gap: 8px;
}

.revision-strip div {
  padding: 10px 12px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--app-radius-md);
  background: var(--el-fill-color-light);
}

.revision-strip span,
.section-header span {
  display: block;
  color: var(--el-text-color-secondary);
  font-size: var(--app-font-size-xs);
}

.revision-strip strong {
  display: block;
  margin-top: 4px;
  color: var(--el-text-color-primary);
  font-size: var(--app-font-size-title);
}

:global(.theme-dark) .revision-strip div,
:global(.dark) .revision-strip div {
  border-color: rgba(115, 115, 115, .14);
  background: var(--md-sys-color-surface-container);
}

:global(.theme-dark) .revision-strip span,
:global(.theme-dark) .section-header span,
:global(.dark) .revision-strip span,
:global(.dark) .section-header span {
  color: var(--md-sys-color-on-surface-variant);
}

:global(.theme-dark) .revision-strip strong,
:global(.dark) .revision-strip strong {
  color: var(--md-sys-color-on-surface);
}

.resume-panel {
  margin: 12px 16px 0;
  padding: 12px;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--app-radius-sm);
  background: var(--md-sys-color-surface-container-low);
}

.resume-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}

.resume-title {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-weight: 600;
  font-size: var(--app-font-size-label);
}

.resume-path {
  margin: 6px 0 0;
  font-size: var(--app-font-size-sm);
  word-break: break-all;
}

.resume-hint {
  margin: 4px 0 0;
  color: var(--md-sys-color-on-surface-variant);
  font-size: var(--app-font-size-sm);
}

.resume-counters {
  margin-top: 6px;
  color: var(--md-sys-color-on-surface-variant);
  font-size: var(--app-font-size-sm);
}

.resume-log {
  max-height: 180px;
  margin-top: 8px;
  padding: 6px 8px;
  overflow: auto;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--app-radius-sm);
  background: var(--md-sys-color-surface-container-lowest);
  font-family: var(--app-font-family-mono);
  font-size: var(--app-font-size-sm);
  line-height: 1.5;
}

.resume-actions {
  display: flex;
  gap: 8px;
  margin-top: 10px;
}

.update-section {
  min-height: 0;
}

.section-header {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  margin-bottom: 8px;
}

.section-header h3 {
  margin: 0;
  color: var(--el-text-color-primary);
  font-size: 15px;
}

.update-table {
  border-radius: var(--app-radius-md);
  overflow: hidden;
}

:global(.theme-dark) .update-table,
:global(.dark) .update-table {
  --el-table-border-color: rgba(115, 115, 115, .12);
  border: 1px solid rgba(115, 115, 115, .1);
}

:global(.theme-dark) .update-table :deep(.el-table__inner-wrapper::before),
:global(.theme-dark) .update-table :deep(.el-table__inner-wrapper::after),
:global(.dark) .update-table :deep(.el-table__inner-wrapper::before),
:global(.dark) .update-table :deep(.el-table__inner-wrapper::after) {
  background-color: rgba(115, 115, 115, .1);
}

:global(.theme-dark) .update-table :deep(th.el-table__cell),
:global(.theme-dark) .update-table :deep(td.el-table__cell),
:global(.dark) .update-table :deep(th.el-table__cell),
:global(.dark) .update-table :deep(td.el-table__cell) {
  border-color: rgba(115, 115, 115, .1);
}

.list-divider {
  height: 1px;
  background: linear-gradient(
    90deg,
    transparent,
    rgba(115, 115, 115, .18),
    transparent
  );
}

.empty-line {
  padding: 12px;
  color: var(--el-text-color-secondary);
  font-size: var(--app-font-size-sm);
  text-align: center;
}

.status-badge {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 72px;
  height: 24px;
  padding: 0 8px;
  border-radius: var(--app-radius-full);
  font-size: var(--app-font-size-xs);
  font-weight: 700;
}

.file-link {
  max-width: 100%;
  padding: 0;
  border: 0;
  background: transparent;
  color: var(--md-sys-color-primary);
  font-family: var(--app-font-family-mono);
  font-size: var(--app-font-size-sm);
  text-align: left;
  cursor: pointer;
}

.file-link:hover {
  text-decoration: underline;
}

:global(.theme-dark) .file-link,
:global(.dark) .file-link {
  color: var(--md-sys-color-primary);
}

:global(.theme-dark) .file-link:hover,
:global(.dark) .file-link:hover {
  color: var(--md-sys-color-on-primary-container);
}

@media (max-width: 860px) {
  .revision-strip {
    grid-template-columns: repeat(2, minmax(120px, 1fr));
  }
}
</style>
