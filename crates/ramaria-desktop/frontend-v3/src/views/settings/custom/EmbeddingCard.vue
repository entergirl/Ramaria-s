<script setup lang="ts">
/**
 * src/views/settings/custom/EmbeddingCard.vue — 嵌入与图片理解卡
 *
 * 职责:
 * - 在嵌入设备 / 图片理解配置键之外补充模型路径的校验与状态
 *
 * 设计特点:
 * - 模型路径随卡片保存一起热加载；校验只探测不写入
 * - 保存成功即热加载，失败保留原状态并给可读原因
 * - 维度与可用性只显示命令返回值，不猜测
 */

import { computed, onMounted, ref } from 'vue'

import BaseButton from '../../../components/base/BaseButton.vue'
import BaseInput from '../../../components/base/BaseInput.vue'
import BaseTag from '../../../components/base/BaseTag.vue'
import { setupApi } from '../../../core/api'
import type { EmbeddingModelView } from '../../../core/api/dto'
import type { JsonValue } from '../../../config/configTypes'
import type { SettingField, SettingGroup } from '../../../config/schema/define'
import { errorMessage } from '../../../core/errors'
import {
  indexCoverageText,
  indexStatusText,
  indexStatusTone,
} from '../../../core/indexStatus'
import { pushToast } from '../../../core/toast'
import { useAppStore } from '../../../stores/app'
import SchemaGroupCard from '../SchemaGroupCard.vue'

const props = withDefaults(
  defineProps<{
    group: SettingGroup
    document: Record<string, JsonValue>
    dirty?: boolean
    saving?: boolean
    error?: string
    resetDisabled?: boolean
    resetHint?: string
    /** 保存本卡的配置键（模型路径另由本卡热加载） */
    saveGroup: () => Promise<boolean>
  }>(),
  {
    dirty: false,
    saving: false,
    error: '',
    resetDisabled: false,
    resetHint: '',
  },
)

const emit = defineEmits<{
  update: [field: SettingField, value: JsonValue]
  reset: []
}>()

const app = useAppStore()

const model = ref<EmbeddingModelView | null>(null)
const modelPath = ref('')
const loading = ref(false)
const validating = ref(false)
const savingPath = ref(false)
const cardError = ref('')
const validation = ref('')

const indexText = computed(() => indexStatusText(app.indexStatus))
const indexTone = computed(() => indexStatusTone(app.indexStatus))
const indexCoverage = computed(() => indexCoverageText(app.indexStatus))

/** 路径改动也计入本卡脏值：页脚保存同时处理配置键与热加载 */
const pathChanged = computed(() => modelPath.value.trim() !== (model.value?.model_path ?? '').trim())

/** 模型指纹：路径 + 维度；未配置时不留空 */
const fingerprint = computed(() => {
  if (!model.value?.model_path) {
    return '未配置'
  }
  const dimension = model.value.dimension ? `@${model.value.dimension}` : ''
  return `${model.value.model_path}${dimension}`
})

const updatedText = computed(() => {
  const at = app.indexStatus?.updated_at
  if (!at) {
    return '未知'
  }
  return new Date(at).toLocaleString('zh-CN', { hour12: false })
})

/** 向量通道可用性：模型可用性与索引状态共同决定，不做单侧推断 */
const vectorText = computed(() => {
  if (!model.value) {
    return '未配置'
  }
  if (!model.value.valid) {
    return '不可用'
  }
  const status = app.indexStatus
  if (!status) {
    return '未知'
  }
  if (status.status === 'building') {
    return '构建中'
  }
  if (status.status === 'ready') {
    return status.failed > 0 ? '部分可用' : '可用'
  }
  if (status.status === 'degraded') {
    return '降级'
  }
  return '未就绪'
})

const vectorTone = computed<'neutral' | 'ok' | 'warn' | 'danger'>(() => {
  switch (vectorText.value) {
    case '可用':
      return 'ok'
    case '部分可用':
    case '未就绪':
      return 'warn'
    case '不可用':
    case '降级':
      return 'danger'
    default:
      return 'neutral'
  }
})

const statusText = computed(() => {
  if (!model.value) {
    return '未配置'
  }
  if (!model.value.valid) {
    return '不可用'
  }
  return model.value.dimension ? `可用 · ${model.value.dimension} 维` : '可用'
})

const statusTone = computed<'ok' | 'warn' | 'neutral'>(() => {
  if (!model.value) {
    return 'neutral'
  }
  return model.value.valid ? 'ok' : 'warn'
})

async function loadModel(): Promise<void> {
  if (loading.value) {
    return
  }
  loading.value = true
  cardError.value = ''
  try {
    model.value = await setupApi.getEmbeddingModel()
    modelPath.value = model.value?.model_path ?? ''
  } catch (cause) {
    cardError.value = errorMessage(cause, '嵌入模型状态读取失败')
  } finally {
    loading.value = false
  }
}

async function validate(): Promise<void> {
  const path = modelPath.value.trim()
  if (!path || validating.value) {
    return
  }
  validating.value = true
  validation.value = ''
  cardError.value = ''
  try {
    const result = await setupApi.validateEmbeddingModel(path)
    validation.value = result.valid
      ? `校验通过${result.dimension ? `（${result.dimension} 维）` : ''}`
      : `校验未通过：${result.reason ?? '未知原因'}`
    pushToast({
      tone: result.valid ? 'success' : 'warn',
      message: result.valid ? '嵌入模型校验通过' : '嵌入模型校验未通过',
    })
  } catch (cause) {
    cardError.value = errorMessage(cause, '嵌入模型校验失败')
  } finally {
    validating.value = false
  }
}

async function savePath(): Promise<boolean> {
  if (savingPath.value) {
    return false
  }
  savingPath.value = true
  cardError.value = ''
  validation.value = ''
  try {
    await setupApi.saveEmbeddingModel(modelPath.value.trim())
    await loadModel()
    return true
  } catch (cause) {
    cardError.value = errorMessage(cause, '嵌入模型保存失败')
    return false
  } finally {
    savingPath.value = false
  }
}

/** 卡片保存：配置键与模型路径一起提交；只改路径时跳过配置写回 */
async function onSave(): Promise<boolean> {
  const configDirty = props.dirty
  const pathDirty = pathChanged.value
  if (!configDirty && !pathDirty) {
    return true
  }
  if (configDirty) {
    const saved = await props.saveGroup()
    if (!saved) {
      return false
    }
  }
  if (pathDirty) {
    const savedPath = await savePath()
    if (!savedPath) {
      return false
    }
    if (!configDirty) {
      pushToast({
        tone: 'success',
        message: modelPath.value.trim() ? '嵌入模型已保存并热加载' : '嵌入模型已卸载',
      })
    }
  }
  return true
}

function onReset(): void {
  modelPath.value = model.value?.model_path ?? ''
  emit('reset')
}

onMounted(() => {
  void loadModel()
})
</script>

<template>
  <SchemaGroupCard
    :group="props.group"
    :document="props.document"
    :dirty="props.dirty || pathChanged"
    :saving="props.saving || savingPath"
    :error="props.error || cardError"
    :reset-disabled="props.resetDisabled"
    :reset-hint="props.resetHint"
    @update="(field, value) => emit('update', field, value)"
    @save="onSave"
    @reset="onReset"
  >
    <template #head>
      <BaseTag :tone="statusTone">{{ statusText }}</BaseTag>
    </template>

    <section class="extra">
      <h4 class="extra-title">嵌入模型路径</h4>
      <BaseInput
        v-model="modelPath"
        label="模型目录"
        placeholder="留空 = 卸载"
        hint="选择包含 safetensors 权重的目录；随卡片保存热加载，校验只探测、不写入。"
      />
      <div class="status-grid">
        <div class="status-cell">
          <span class="status-label">索引状态</span>
          <BaseTag :tone="indexTone">{{ indexText }}</BaseTag>
        </div>
        <div class="status-cell">
          <span class="status-label">索引覆盖</span>
          <b class="status-value">{{ indexCoverage }}</b>
        </div>
        <div class="status-cell">
          <span class="status-label">向量通道</span>
          <BaseTag :tone="vectorTone">{{ vectorText }}</BaseTag>
        </div>
      </div>

      <div class="meta-row">
        <span class="status-label">模型指纹</span>
        <b class="status-value">{{ fingerprint }}</b>
      </div>
      <div class="meta-row">
        <span class="status-label">最近更新</span>
        <b class="status-value">{{ updatedText }}</b>
      </div>

      <p v-if="validation" class="extra-note">{{ validation }}</p>
      <p class="extra-note">
        缺失嵌入时语义通道关闭，召回退回关键词与图谱，主流程不阻塞。
      </p>
    </section>

    <template #footExtra>
      <BaseButton
        variant="ghost"
        size="sm"
        :loading="validating"
        :disabled="!modelPath.trim()"
        @click="validate"
      >
        校验向量模型
      </BaseButton>
    </template>
  </SchemaGroupCard>
</template>

<style scoped>
.extra {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
  padding-top: var(--space-10);
  border-top: 1px dashed var(--border);
}

.extra-title {
  margin: 0;
  color: var(--text-2);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
}

.status-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(112px, 1fr));
  gap: var(--space-10) var(--space-14);
}

.status-cell {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: var(--space-4);
  min-width: 0;
}

.status-label {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.status-value {
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
  line-height: 1.5;
}

.meta-row {
  display: flex;
  align-items: baseline;
  gap: var(--space-10);
}

.meta-row .status-label {
  flex: none;
  width: 64px;
}

.meta-row .status-value {
  flex: 1;
  min-width: 0;
  word-break: break-all;
}

.extra-note {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
}
</style>
