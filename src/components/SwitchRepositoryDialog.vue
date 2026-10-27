<template>
  <el-dialog
    :model-value="modelValue"
    :title="$t('workspace.switchTitle')"
    width="560px"
    class="switch-repository-dialog"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <p class="dialog-description">{{ $t('workspace.switchDescription') }}</p>
    <el-form label-position="top">
      <el-form-item :label="$t('workspace.switchField')">
        <el-input v-model="url" :placeholder="currentUrl || 'https://svn.example.com/repo/trunk'" clearable />
      </el-form-item>
    </el-form>
    <p v-if="currentUrl" class="dialog-current">
      <span>{{ $t('workspace.switchCurrent') }}</span>
      <code>{{ currentUrl }}</code>
    </p>
    <template #footer>
      <el-button :disabled="busy" @click="close">{{ $t('common.cancel') }}</el-button>
      <el-button type="primary" :loading="busy" :disabled="!canSubmit" @click="submit">
        {{ $t('workspace.switchAction') }}
      </el-button>
    </template>
  </el-dialog>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ElMessage } from 'element-plus/es/components/message/index'
import { svnSwitch } from '@/api/svn'
import { useWorkspace } from '@/composables/useWorkspace'
import { errorMessage } from '@/utils/errors'

const props = defineProps<{
  modelValue: boolean
  workspacePath: string | null
  currentUrl: string
}>()
const emit = defineEmits<{ 'update:modelValue': [boolean]; done: [] }>()

const { t } = useI18n()
const { refreshStatusAfterMutation } = useWorkspace()

const url = ref('')
const busy = ref(false)

watch(() => props.modelValue, visible => {
  if (visible) url.value = props.currentUrl
})

const canSubmit = computed(() => Boolean(props.workspacePath) && url.value.trim().length > 0
  && url.value.trim() !== props.currentUrl && !busy.value)

const close = () => emit('update:modelValue', false)

const submit = async () => {
  const workspacePath = props.workspacePath
  const target = url.value.trim()
  if (!workspacePath || !target || busy.value) return
  busy.value = true
  try {
    await svnSwitch(workspacePath, target)
    await refreshStatusAfterMutation()
    ElMessage.success(t('workspace.switchDone'))
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

.dialog-current {
  margin: 4px 0 0;
  color: var(--md-sys-color-on-surface-variant);
  font-size: var(--app-font-size-sm);
}

.dialog-current code {
  display: block;
  margin-top: 4px;
  word-break: break-all;
}
</style>
