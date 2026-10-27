<template>
  <el-dialog
    :model-value="modelValue"
    :title="$t('workspace.mergeTitle')"
    width="560px"
    class="merge-dialog"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <p class="dialog-description">{{ $t('workspace.mergeDescription') }}</p>
    <el-form label-position="top">
      <el-form-item :label="$t('workspace.mergeSource')">
        <el-input v-model="source" :placeholder="currentUrl || 'https://svn.example.com/repo/branches/feature'" clearable />
      </el-form-item>
      <div class="revision-row">
        <el-form-item :label="$t('workspace.mergeStartRevision')">
          <el-input-number v-model="startRevision" :min="1" :step="1" controls-position="right" />
        </el-form-item>
        <el-form-item :label="$t('workspace.mergeEndRevision')">
          <el-input-number v-model="endRevision" :min="1" :step="1" controls-position="right" />
        </el-form-item>
      </div>
    </el-form>
    <p v-if="!rangeValid" class="dialog-warning">{{ $t('workspace.mergeRangeInvalid') }}</p>
    <p class="dialog-hint">{{ $t('workspace.mergeHint') }}</p>
    <template #footer>
      <el-button :disabled="busy" @click="close">{{ $t('common.cancel') }}</el-button>
      <el-button type="primary" :loading="busy" :disabled="!canSubmit" @click="submit">
        {{ $t('workspace.mergeAction') }}
      </el-button>
    </template>
  </el-dialog>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus/es/components/message/index'
import { svnMerge } from '@/api/svn'
import { useWorkspace } from '@/composables/useWorkspace'
import { errorMessage } from '@/utils/errors'
import { defaultMergeStart, isValidRevisionRange } from '@/utils/revisionRange'
import type { SvnInfo } from '@/types'

const props = defineProps<{
  modelValue: boolean
  workspacePath: string | null
  currentUrl: string
  svnInfo: SvnInfo | null
}>()
const emit = defineEmits<{ 'update:modelValue': [boolean]; done: [] }>()

const { t } = useI18n()
const { refreshStatusAfterMutation } = useWorkspace()

const source = ref('')
const startRevision = ref(1)
const endRevision = ref(1)
const busy = ref(false)

watch(() => props.modelValue, visible => {
  if (!visible) return
  source.value = props.currentUrl
  startRevision.value = defaultMergeStart(props.svnInfo)
  endRevision.value = defaultMergeStart(props.svnInfo)
})

const rangeValid = computed(() => isValidRevisionRange(startRevision.value, endRevision.value))
const canSubmit = computed(() => Boolean(props.workspacePath) && source.value.trim().length > 0
  && rangeValid.value && !busy.value)

const close = () => emit('update:modelValue', false)

const submit = async () => {
  const workspacePath = props.workspacePath
  const target = source.value.trim()
  if (!workspacePath || !target || !canSubmit.value) return
  busy.value = true
  try {
    await svnMerge(workspacePath, target, startRevision.value, endRevision.value)
    await refreshStatusAfterMutation()
    ElMessage.success(t('workspace.mergeDone'))
    emit('done')
    close()
  } catch (err) {
    ElMessage.error(errorMessage(err))
  } finally {
    busy.value = false
  }
}
</script>

<style scoped>
.dialog-description {
  margin: 0 0 12px;
  color: var(--md-sys-color-on-surface-variant);
  font-size: var(--app-font-size-sm);
  line-height: var(--app-line-height-relaxed);
}

.revision-row {
  display: flex;
  gap: 16px;
}

.dialog-warning {
  margin: 0 0 8px;
  color: var(--app-color-status-error-text);
  font-size: var(--app-font-size-sm);
}

.dialog-hint {
  margin: 0;
  color: var(--md-sys-color-on-surface-variant);
  font-size: var(--app-font-size-sm);
  line-height: var(--app-line-height-relaxed);
}
</style>
