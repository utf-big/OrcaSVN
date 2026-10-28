<template>
  <div class="checkout-view">
    <el-card class="checkout-card animate-fade-in">
      <template #header>
        <div class="card-header">
          <span class="card-title">
            <el-icon><Download /></el-icon>
            {{ $t('checkout.title') }}
          </span>
        </div>
      </template>

      <el-form
        :model="form"
        label-width="120px"
        class="checkout-form"
        label-position="left"
        require-asterisk-position="right"
      >
        <el-form-item :label="$t('checkout.repositoryUrl')" required class="form-item">
          <el-input
            v-model="form.url"
            :placeholder="$t('checkout.repositoryUrlPlaceholder')"
            class="checkout-control"
            clearable
          >
            <template #prefix>
              <el-icon><Link /></el-icon>
            </template>
          </el-input>
        </el-form-item>

        <el-form-item :label="$t('checkout.targetDirectory')" required class="form-item">
          <el-input
            v-model="form.path"
            :placeholder="$t('checkout.selectTargetDirectory')"
            class="checkout-control"
            clearable
          >
            <template #prefix>
              <el-icon><Folder /></el-icon>
            </template>
            <template #append>
              <el-button @click="selectDirectory">
                <el-icon><FolderOpened /></el-icon>
                {{ $t('common.browse') }}
              </el-button>
            </template>
          </el-input>
        </el-form-item>

        <el-form-item :label="$t('checkout.revision')" class="form-item">
          <el-input
            v-model.number="form.revision"
            type="number"
            :placeholder="$t('checkout.revisionPlaceholder')"
            class="checkout-control revision-control"
            clearable
          >
            <template #prefix>
              <el-icon><PriceTag /></el-icon>
            </template>
          </el-input>
        </el-form-item>

        <el-form-item class="form-actions">
          <el-button type="primary" @click="doCheckout" :loading="loading" size="large">
            <el-icon><Download /></el-icon>
            {{ $t('checkout.startCheckout') }}
          </el-button>
          <el-button v-if="loading" type="danger" plain @click="cancelCheckout" size="large">
            <el-icon><Close /></el-icon>
            {{ $t('common.cancel') }}
          </el-button>
          <el-button @click="resetForm" size="large">
            <el-icon><RefreshLeft /></el-icon>
            {{ $t('common.reset') }}
          </el-button>
        </el-form-item>
      </el-form>

      <div v-if="state !== 'idle'" class="output-area animate-fade-in">
        <div class="output-header">
          <span class="output-title">
            <el-icon><Document /></el-icon>
            {{ $t('checkout.progressTitle') }}
          </span>
          <el-tag :type="statusTag" size="small">{{ statusText }}</el-tag>
        </div>

        <div class="progress-summary">
          <span>{{ $t('checkout.progressCounters', progress.counters) }}</span>
          <span v-if="countedItems(progress.counters) > 0">· {{ $t('checkout.progressTotal', { count: countedItems(progress.counters) }) }}</span>
          <span>· {{ $t('checkout.elapsed', { seconds: elapsedSeconds }) }}</span>
          <span v-if="attempts > 1">· {{ $t('checkout.attempts', { attempt: attempts }) }}</span>
        </div>

        <p v-if="failureText" class="progress-failure">{{ failureText }}</p>

        <div ref="logBox" class="progress-log" role="log" aria-live="polite">
          <p v-if="!progress.lines.length" class="progress-empty">{{ $t('checkout.progressEmpty') }}</p>
          <div v-for="(line, index) in progress.lines" :key="index" class="progress-line" :class="`is-${line.kind}`">{{ line.text }}</div>
        </div>

        <p v-if="missingFiles.length" class="progress-failure">
          {{ $t('checkout.missingFiles', { count: missingFiles.length }) }}
          <span class="salvage-list">{{ missingFiles.join('、') }}</span>
        </p>
        <p v-if="salvageFailed.length" class="progress-failure">
          {{ $t('checkout.salvageIncomplete', { count: salvageFailed.length }) }}
          <span class="salvage-list">{{ salvageFailed.join('、') }}</span>
        </p>
        <div class="progress-footer">
          <span>{{ $t('checkout.progressLines', { count: progress.total, shown: progress.lines.length }) }}</span>
          <div class="progress-actions">
            <el-button v-if="state === 'failed'" size="small" :loading="loading" @click="doCheckout">
              <el-icon><RefreshRight /></el-icon>
              {{ $t('checkout.retry') }}
            </el-button>
            <el-button v-if="state === 'failed'" size="small" type="primary" :loading="loading" @click="doSalvage">
              <el-icon><Download /></el-icon>
              {{ $t('checkout.salvage') }}
            </el-button>
            <el-button v-if="state === 'failed' || state === 'cancelled'" size="small" :loading="cleaning"
              :title="$t('workspace.quickCleanupHint')" @click="doCleanup">
              <el-icon><Brush /></el-icon>
              {{ $t('common.cleanup') }}
            </el-button>
            <el-button v-if="state === 'success' && workspaceStore.currentPath === form.path" size="small" type="primary"
              @click="router.push({ name: 'workspace' })">
              <el-icon><FolderOpened /></el-icon>
              {{ $t('workspace.openWorkspace') }}
            </el-button>
          </div>
        </div>
      </div>
    </el-card>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onUnmounted, reactive, ref } from 'vue'
import { useRouter } from 'vue-router'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { useWorkspaceStore } from '@/stores/workspace'
import { useCheckoutStore } from '@/stores/checkout'
import { isWorkingCopy, svnCancel, svnCheckoutSalvage, svnCheckoutStream, svnCleanup } from '@/api/svn'
import { useWorkspace } from '@/composables/useWorkspace'
import { waitForWorkingCopy } from '@/utils/workspaceAttach'
import { open } from '@tauri-apps/plugin-dialog'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus/es/components/message/index'
import { errorMessage } from '@/utils/errors'
import {
  countedItems,
  createProgressState,
  pushCheckoutLine,
  type CheckoutLineKind,
} from '@/utils/checkoutProgress'

const { t } = useI18n()
const router = useRouter()
const workspaceStore = useWorkspaceStore()
const checkoutStore = useCheckoutStore()
const { refreshStatusAfterMutation } = useWorkspace()

const form = reactive({
  url: '',
  path: '',
  revision: undefined as number | undefined
})

const loading = ref(false)
const cancelled = ref(false)
// Progress is page state only: switching away destroys the view, so nothing is
// persisted and nothing survives a navigation.
const state = ref<'idle' | 'running' | 'success' | 'failed' | 'cancelled'>('idle')
const progress = reactive(createProgressState())
const failureText = ref('')
const attempts = ref(0)
const salvageFailed = ref<string[]>([])
const missingFiles = ref<string[]>([])
const cleaning = ref(false)
const elapsedSeconds = ref(0)
const logBox = ref<HTMLElement | null>(null)
let unlisten: UnlistenFn | null = null
let elapsedTimer: number | undefined

const statusTag = computed(() => {
  if (state.value === 'success') return 'success'
  if (state.value === 'failed') return 'danger'
  if (state.value === 'cancelled') return 'info'
  return 'primary'
})
const statusText = computed(() => {
  switch (state.value) {
    case 'running': return t('checkout.statusRunning')
    case 'success': return t('checkout.statusSuccess')
    case 'failed': return t('checkout.statusFailed')
    case 'cancelled': return t('checkout.statusCancelled')
    default: return t('checkout.progressTitle')
  }
})

const scrollLogToBottom = async () => {
  await nextTick()
  const box = logBox.value
  if (box) box.scrollTop = box.scrollHeight
}

const startElapsedTimer = () => {
  elapsedSeconds.value = 0
  stopElapsedTimer()
  elapsedTimer = window.setInterval(() => { elapsedSeconds.value += 1 }, 1000)
}

const stopElapsedTimer = () => {
  if (elapsedTimer !== undefined) {
    window.clearInterval(elapsedTimer)
    elapsedTimer = undefined
  }
}

const stopListening = () => {
  unlisten?.()
  unlisten = null
}

const selectDirectory = async () => {
  const selected = await open({
    directory: true,
    multiple: false,
    title: t('dialog.selectTargetDirectory')
  })

  if (selected) {
    form.path = Array.isArray(selected) ? selected[0] : selected
  }
}

const doCheckout = async () => {
  if (!form.url || !form.path) {
    return
  }

  cancelled.value = false
  failureText.value = ''
  attempts.value = 0
  Object.assign(progress, createProgressState())
  salvageFailed.value = []
  missingFiles.value = []
  state.value = 'running'
  loading.value = true
  checkoutStore.begin(form.url, form.path, form.revision)
  // Switch the workspace as soon as SVN has created the working copy, which it
  // does before transferring files. Switching sooner leaves every SVN-backed
  // view querying a directory that is not a working copy yet (E155007).
  void attachWorkspaceWhenReady(form.path)
  startElapsedTimer()
  stopListening()
  unlisten = await listen<{ kind: CheckoutLineKind; line: string }>('checkout-progress', event => {
    pushCheckoutLine(progress, event.payload.kind, event.payload.line)
    void scrollLogToBottom()
  })

  try {
    const report = await svnCheckoutStream(form.url, form.path, form.revision)
    attempts.value = report.attempts
    salvageFailed.value = report.failed ?? []
    missingFiles.value = report.missingFiles ?? []
    state.value = 'success'
    checkoutStore.complete()
    // The workspace is already current; fill in the file list it did not have
    // while the checkout was running.
    await refreshStatusAfterMutation()

    // Keep the report on screen when something is missing, so the user can read
    // which files the server could not deliver.
    if (!report.salvaged && (report.missingFiles?.length ?? 0) === 0) {
      setTimeout(() => {
        router.push({ name: 'workspace' })
      }, 1500)
    }
  } catch (err) {
    // A cancelled checkout leaves a partial directory behind; report it as a
    // cancellation instead of an SVN failure so the message stays actionable.
    if (cancelled.value) {
      state.value = 'cancelled'
      checkoutStore.markCancelled()
      failureText.value = t('checkout.cancelled')
    } else {
      state.value = 'failed'
      checkoutStore.markFailed()
      failureText.value = `${t('common.error')}：${errorMessage(err)}`
    }
  } finally {
    loading.value = false
    cancelled.value = false
    cancelWorkspaceAttach()
    stopElapsedTimer()
    stopListening()
  }
}

/**
 * The server could not serve part of the repository (a damaged revision file).
 * Fetch everything else so the user can keep working, and list what is missing.
 */
const doSalvage = async () => {
  if (!form.url || !form.path) return
  cancelled.value = false
  failureText.value = ''
  salvageFailed.value = []
  missingFiles.value = []
  Object.assign(progress, createProgressState())
  state.value = 'running'
  checkoutStore.begin(form.url, form.path, form.revision)
  void attachWorkspaceWhenReady(form.path)
  loading.value = true
  startElapsedTimer()
  stopListening()
  unlisten = await listen<{ kind: CheckoutLineKind; line: string }>('checkout-progress', event => {
    pushCheckoutLine(progress, event.payload.kind, event.payload.line)
    void scrollLogToBottom()
  })

  try {
    const report = await svnCheckoutSalvage(form.url, form.path)
    salvageFailed.value = report.failed
    missingFiles.value = report.missingFiles ?? []
    state.value = 'success'
    checkoutStore.complete()
    workspaceStore.setCurrentPath(form.path)
    await refreshStatusAfterMutation()
  } catch (err) {
    if (cancelled.value) {
      state.value = 'cancelled'
      checkoutStore.markCancelled()
      failureText.value = t('checkout.cancelled')
    } else {
      state.value = 'failed'
      checkoutStore.markFailed()
      failureText.value = `${t('common.error')}：${errorMessage(err)}`
    }
  } finally {
    loading.value = false
    cancelled.value = false
    cancelWorkspaceAttach()
    stopElapsedTimer()
    stopListening()
  }
}

let attachGeneration = 0

/**
 * Makes the target the current workspace once SVN has created `.svn` in it, and
 * marks its file list as unverified until the first scan runs.
 */
const attachWorkspaceWhenReady = async (path: string) => {
  const generation = ++attachGeneration
  const ready = await waitForWorkingCopy(() => isWorkingCopy(path), {
    shouldStop: () => generation !== attachGeneration,
  })
  if (!ready || generation !== attachGeneration) return
  workspaceStore.setCurrentPath(path)
  workspaceStore.setStatusList([])
  workspaceStore.setSvnInfo(null)
  workspaceStore.setStatusIsStale(true)
}

/** Stops a pending switch, e.g. when the checkout ended without a working copy. */
const cancelWorkspaceAttach = () => {
  attachGeneration += 1
}

/** Clears a lock left behind by the interrupted checkout. */
const doCleanup = async () => {
  const path = form.path
  if (!path || cleaning.value) return
  cleaning.value = true
  try {
    await svnCleanup(path)
    ElMessage.success(`${t('common.cleanup')} ${t('common.success')}`)
  } catch (err) {
    ElMessage.error(`${t('common.error')}：${errorMessage(err)}`)
  } finally {
    cleaning.value = false
  }
}

const cancelCheckout = async () => {
  if (!form.path) return
  cancelled.value = true
  await svnCancel(form.path).catch(() => false)
}

const resetForm = () => {
  form.url = ''
  form.path = ''
  form.revision = undefined
  state.value = 'idle'
  failureText.value = ''
  Object.assign(progress, createProgressState())
}

onUnmounted(() => {
  cancelWorkspaceAttach()
  stopElapsedTimer()
  stopListening()
})
</script>

<style scoped>
.checkout-view {
  max-width: 800px;
  margin: 0 auto;
  background: var(--md-sys-color-surface);
}

.checkout-card {
  border-radius: var(--app-radius-lg);
  background: var(--md-sys-color-surface);
}

.card-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.card-title {
  display: inline-flex;
  align-items: center;
  gap: var(--app-spacing-sm);
  font-weight: 700;
}

.checkout-form {
  margin-top: var(--app-spacing-md);
}

.checkout-form :deep(.el-form-item__label) {
  position: relative;
  display: block;
  overflow: hidden;
  padding-right: 20px;
  line-height: 32px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.checkout-form :deep(.el-form-item.is-required .el-form-item__label::before) {
  display: none;
}

.checkout-form :deep(.el-form-item.is-required .el-form-item__label::after) {
  content: "*";
  margin-left: 5px;
  color: var(--el-color-danger);
}

.form-item {
  margin-bottom: var(--app-spacing-lg);
}

.checkout-control {
  width: 520px;
  max-width: 100%;
}

.revision-control {
  width: 240px;
}

.form-actions {
  margin-bottom: 0;
  margin-top: var(--app-spacing-lg);
}

.output-area {
  margin-top: var(--app-spacing-lg);
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--app-radius-md);
  overflow: hidden;
}

.output-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--app-spacing) var(--app-spacing-md);
  background: var(--el-fill-color-light);
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.output-title {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-weight: 600;
  font-size: var(--app-font-size-label);
  color: var(--el-text-color-regular);
}

.progress-summary {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  padding: 6px 12px;
  color: var(--md-sys-color-on-surface-variant);
  font-size: var(--app-font-size-sm);
}

.progress-failure {
  margin: 0 12px 8px;
  padding: 8px 10px;
  border: 1px solid var(--md-sys-color-error);
  border-radius: var(--app-radius-sm);
  background: var(--app-color-status-error-bg);
  color: var(--app-color-status-error-text);
  font-size: var(--app-font-size-sm);
  white-space: pre-line;
  word-break: break-word;
}

.progress-log {
  max-height: 280px;
  min-height: 120px;
  margin: 0 12px;
  padding: 8px 10px;
  overflow: auto;
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--app-radius-sm);
  background: var(--md-sys-color-surface-container-lowest);
  font-family: var(--app-font-family-mono);
  font-size: var(--app-font-size-sm);
  line-height: 1.5;
}

.progress-line {
  white-space: pre-wrap;
  word-break: break-all;
}

.progress-line.is-err {
  color: var(--app-color-status-error-text);
}

.progress-line.is-status {
  color: var(--md-sys-color-primary);
}

.progress-empty {
  margin: 0;
  color: var(--md-sys-color-on-surface-variant);
}

.progress-actions {
  display: flex;
  gap: 8px;
}

.salvage-list {
  display: block;
  margin-top: 4px;
  font-family: var(--app-font-family-mono);
  word-break: break-all;
}

.progress-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding: 8px 12px 2px;
  color: var(--md-sys-color-on-surface-variant);
  font-size: var(--app-font-size-sm);
}

.output-textarea {
  font-family: var(--app-font-family-mono);
}

:deep(.output-textarea .el-textarea__inner) {
  border: none;
  border-radius: 0;
  padding: var(--app-spacing-md);
}

@media (max-width: 640px) {
  .checkout-view {
    padding: 0 var(--app-spacing);
  }
  
  .checkout-form :deep(.el-form-item) {
    flex-direction: column;
  }
  
  .checkout-form :deep(.el-form-item__label) {
    text-align: left;
    padding-bottom: 4px;
    width: 100% !important;
  }

  .checkout-control,
  .revision-control {
    width: 100%;
  }
  
  .form-actions {
    display: flex;
    flex-direction: column;
  }
  
  .form-actions .el-button {
    width: 100%;
  }
}
</style>
