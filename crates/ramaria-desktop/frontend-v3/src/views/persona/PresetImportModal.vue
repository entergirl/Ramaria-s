<script setup lang="ts">
/**
 * src/views/persona/PresetImportModal.vue — 预设导入（两段式）
 *
 * 职责:
 * - 选文件 → 预览裁决文本与拆解状态 → 确认导入 → 回传结果
 *
 * 设计特点:
 * - 预览只调 dry-run 命令，不写库、不调用模型；正文由服务层渲染，前端不重算
 * - 切换 LLM 拆解开关即重新预览，保证预览与将要执行的裁决一致
 * - 导入执行中锁定关闭与重选，避免半途换文件
 */
import { computed, ref, watch } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseModal from '../../components/base/BaseModal.vue'
import BaseSwitch from '../../components/base/BaseSwitch.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import { personaApi, systemApi } from '../../core/api'
import type { PresetImportOutcome, PresetImportPreview } from '../../core/api/dto'
import { errorMessage } from '../../core/errors'
import { presetModeLabel } from '../../core/persona/format'

const props = defineProps<{
  open: boolean
}>()

const emit = defineEmits<{
  close: []
  imported: [outcome: PresetImportOutcome]
}>()

const filePath = ref('')
const preview = ref<PresetImportPreview | null>(null)
const loading = ref(false)
const importing = ref(false)
const error = ref('')
const noAi = ref(false)

const fileName = computed(() => {
  if (filePath.value === '') {
    return ''
  }
  const parts = filePath.value.split(/[\\/]/)
  return parts[parts.length - 1] ?? filePath.value
})
const uidText = computed(() => preview.value?.uid || '自动生成')
const canImport = computed(
  () => preview.value !== null && !loading.value && !importing.value && error.value === '',
)

watch(
  () => props.open,
  (open) => {
    if (!open) {
      return
    }
    filePath.value = ''
    preview.value = null
    loading.value = false
    importing.value = false
    error.value = ''
    noAi.value = false
  },
)

function requestClose(): void {
  if (importing.value) {
    return
  }
  emit('close')
}

async function chooseFile(): Promise<void> {
  if (loading.value || importing.value) {
    return
  }
  error.value = ''
  try {
    const picked = await systemApi.openFileDialog({
      title: '选择人格预设',
      filterName: '人格预设',
      extensions: ['toml', 'json', 'png'],
    })
    if (!picked) {
      return
    }
    filePath.value = picked
    preview.value = null
    await runPreview()
  } catch (failure) {
    error.value = errorMessage(failure, '选择文件失败')
  }
}

async function runPreview(): Promise<void> {
  if (filePath.value === '' || loading.value) {
    return
  }
  loading.value = true
  error.value = ''
  try {
    preview.value = await personaApi.previewImport(filePath.value, null, noAi.value)
  } catch (failure) {
    preview.value = null
    error.value = errorMessage(failure, '预设预览失败')
  } finally {
    loading.value = false
  }
}

async function toggleAi(enabled: boolean): Promise<void> {
  noAi.value = !enabled
  preview.value = null
  await runPreview()
}

async function confirm(): Promise<void> {
  if (!canImport.value || filePath.value === '') {
    return
  }
  importing.value = true
  error.value = ''
  try {
    const outcome = await personaApi.importPreset(filePath.value, null, noAi.value)
    emit('imported', outcome)
  } catch (failure) {
    error.value = errorMessage(failure, '预设导入失败')
  } finally {
    importing.value = false
  }
}
</script>

<template>
  <BaseModal
    :open="open"
    title="导入人格预设"
    :loading="importing"
    @close="requestClose"
  >
    <div class="preset-body">
      <div class="file-row">
        <span class="file-icon" aria-hidden="true">
          <svg viewBox="0 0 24 24">
            <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
            <path d="M14 2v6h6" />
          </svg>
        </span>
        <div class="file-text">
          <div class="file-name" :class="{ 'is-empty': fileName === '' }">
            {{ fileName || '尚未选择预设' }}
          </div>
          <div class="file-hint">支持 .toml / .json / .png 预设；预览不写库、不调用模型。</div>
        </div>
        <span class="spacer" />
        <BaseButton
          variant="ghost"
          size="sm"
          :disabled="importing"
          @click="chooseFile"
        >
          {{ fileName ? '重选文件' : '选择文件' }}
        </BaseButton>
      </div>

      <div v-if="error" class="preset-error" role="alert">
        <span>{{ error }}</span>
        <BaseButton
          v-if="filePath && preview === null"
          variant="ghost"
          size="sm"
          @click="runPreview"
        >
          重试
        </BaseButton>
      </div>

      <div v-if="loading" class="preset-state">正在解析预设…</div>

      <template v-else-if="preview">
        <div class="preset-head">
          <div class="preset-name">{{ preview.name }}</div>
          <div class="preset-tags">
            <BaseTag tone="neutral">UID {{ uidText }}</BaseTag>
            <BaseTag :tone="preview.mode === 'merge' ? 'accent' : 'brand'">
              {{ presetModeLabel(preview.mode) }}
            </BaseTag>
            <BaseTag :tone="preview.decompose_enabled ? 'ok' : 'neutral'">
              拆解 {{ preview.decompose_enabled ? '已启用' : '已关闭' }}
            </BaseTag>
            <BaseTag v-if="!preview.privacy_confirmed" tone="warn">隐私未确认</BaseTag>
          </div>
        </div>

        <p v-if="!preview.privacy_confirmed" class="preset-note">
          未确认云端隐私范围时 LLM 拆解会跳过；可在设置完成确认后重新预览。
        </p>

        <pre class="preset-text">{{ preview.text }}</pre>

        <div class="preset-switch">
          <BaseSwitch
            :model-value="!noAi"
            label="LLM 拆解事实与特质"
            :disabled="loading || importing"
            @update:model-value="toggleAi"
          />
          <span class="switch-hint">关闭后只写入注册列与文本块，不产生事实与特质条目。</span>
        </div>
      </template>

      <div v-else-if="!error" class="preset-state">
        选择文件后显示将写入的内容；确认前不会产生任何改动。
      </div>
    </div>

    <template #foot>
      <span class="spacer" />
      <BaseButton variant="ghost" :disabled="importing" @click="requestClose">取消</BaseButton>
      <BaseButton :disabled="!canImport" :loading="importing" @click="confirm">
        导入人格
      </BaseButton>
    </template>
  </BaseModal>
</template>

<style scoped>
.preset-body {
  display: flex;
  flex-direction: column;
  gap: var(--space-12);
}

.file-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-10);
  padding: var(--space-10) var(--space-12);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.file-icon {
  display: grid;
  flex: none;
  place-items: center;
  width: 32px;
  height: 32px;
  border-radius: var(--radius-tag);
  background: var(--bg-card);
  color: var(--brand-text);
}

.file-icon svg {
  width: 17px;
  height: 17px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.file-text {
  min-width: 0;
}

.file-name {
  overflow: hidden;
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.file-name.is-empty {
  color: var(--text-3);
  font-weight: var(--fw-regular);
}

.file-hint {
  margin-top: 2px;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.spacer {
  flex: 1;
}

.preset-error {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
  padding: var(--space-8) var(--space-12);
  border: 1px solid var(--danger-line);
  border-radius: var(--radius-tag);
  background: var(--danger-surface);
  color: var(--danger);
  font-size: var(--fs-small);
}

.preset-state {
  padding: var(--space-16) var(--space-12);
  border: 1px dashed var(--border-strong);
  border-radius: var(--radius-inner);
  color: var(--text-3);
  font-size: var(--fs-small);
  text-align: center;
}

.preset-head {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
}

.preset-name {
  color: var(--text-1);
  font-size: var(--fs-body-lg);
  font-weight: var(--fw-bold);
}

.preset-tags {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-6);
}

.preset-note {
  margin: 0;
  padding: var(--space-8) var(--space-10);
  border: 1px solid var(--warn-line);
  border-radius: var(--radius-tag);
  background: var(--warn-surface);
  color: var(--warn);
  font-size: var(--fs-caption);
  line-height: 1.6;
}

.preset-text {
  max-height: 260px;
  margin: 0;
  padding: var(--space-10) var(--space-12);
  overflow: auto;
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
  color: var(--text-2);
  font-family: var(--font-mono);
  font-size: var(--fs-code);
  line-height: 1.65;
  white-space: pre-wrap;
  word-break: break-word;
}

.preset-switch {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-10);
}

.switch-hint {
  color: var(--text-3);
  font-size: var(--fs-caption);
}
</style>
