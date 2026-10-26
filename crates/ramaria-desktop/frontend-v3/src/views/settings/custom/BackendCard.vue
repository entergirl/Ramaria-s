<script setup lang="ts">
/**
 * src/views/settings/custom/BackendCard.vue — 后端配置卡
 *
 * 职责:
 * - 在 `[backend]` 配置键之外补充 API key 写入与连接测试
 *
 * 设计特点:
 * - 密钥随卡片保存一起写入 keychain，不设独立动作
 * - 测试连接前若表单或密钥有改动，先保存再探测（与后端「探测已保存配置」口径一致）
 * - 密钥只显示遮罩状态，输入新值即覆盖，不回显明文
 */

import { computed, onMounted, ref, watch } from 'vue'

import BaseButton from '../../../components/base/BaseButton.vue'
import BaseInput from '../../../components/base/BaseInput.vue'
import { configApi, setupApi } from '../../../core/api'
import type { BackendConfig } from '../../../core/api/dto'
import { readPath } from '../../../config/configDraft'
import type { JsonValue } from '../../../config/configTypes'
import type { SettingField, SettingGroup } from '../../../config/schema/define'
import { errorMessage } from '../../../core/errors'
import { pushToast } from '../../../core/toast'
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
    /** 保存本卡（测试连接前复用） */
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

const apiKey = ref('')
const backend = ref<BackendConfig | null>(null)
const writing = ref(false)
const testing = ref(false)
const cardError = ref('')

const provider = computed(() => String(readPath(props.document, ['backend', 'provider']) ?? ''))
const isOnline = computed(() => provider.value === 'deepseek' || provider.value === 'openai')
const canTest = computed(() => !testing.value)

const maskedKey = computed(() => backend.value?.apiKeyMasked ?? null)

/** 密钥输入未落盘时也算本卡有改动，页脚保存随配置一起提交 */
const extraDirty = computed(() => isOnline.value && apiKey.value.trim().length > 0)

const keyPlaceholder = computed(() => {
  if (!isOnline.value) {
    return '本地模式不需要密钥'
  }
  return maskedKey.value ? `已配置（${maskedKey.value}），输入新值覆盖` : '未配置'
})

watch(isOnline, (online) => {
  if (!online) {
    apiKey.value = ''
  }
})

/** 密钥状态：本地模式不参与云端确认，单独给文案 */
const keyText = computed(() => {
  if (!isOnline.value) {
    return '本地模式不需要'
  }
  return maskedKey.value ? '已配置' : '未配置'
})

/** 后端声明的接入能力，未读取到时不猜 */
const capabilityText = computed(() => {
  if (!backend.value) {
    return '读取中…'
  }
  const items: string[] = []
  if (backend.value.supportsStreaming) {
    items.push('流式')
  }
  if (backend.value.supportsJsonMode) {
    items.push('JSON')
  }
  return items.length > 0 ? items.join(' · ') : '基础'
})

const windowText = computed(() => {
  if (!backend.value) {
    return '读取中…'
  }
  return `上下文 ${backend.value.contextWindow} · 输出 ${backend.value.maxOutputTokens}`
})

async function loadBackend(): Promise<void> {
  try {
    backend.value = await configApi.getBackend()
  } catch (cause) {
    cardError.value = errorMessage(cause, '后端配置读取失败')
  }
}

/** 写入密钥；本地模式或空值直接视为成功 */
async function writeKey(): Promise<boolean> {
  const key = apiKey.value.trim()
  if (!isOnline.value || !key) {
    return true
  }
  if (writing.value) {
    return false
  }
  writing.value = true
  cardError.value = ''
  try {
    await configApi.updateBackend(
      provider.value,
      String(readPath(props.document, ['backend', 'model_id']) ?? ''),
      String(readPath(props.document, ['backend', 'base_url']) ?? ''),
      key,
    )
    apiKey.value = ''
    await loadBackend()
    return true
  } catch (cause) {
    const message = errorMessage(cause, '密钥写入失败')
    cardError.value = message
    pushToast({ tone: 'danger', message })
    return false
  } finally {
    writing.value = false
  }
}

/** 卡片保存：先落配置键，再落密钥；只改密钥时跳过配置写回 */
async function onSave(): Promise<boolean> {
  const configDirty = props.dirty
  const keyDirty = extraDirty.value
  if (!configDirty && !keyDirty) {
    return true
  }
  if (configDirty) {
    const saved = await props.saveGroup()
    if (!saved) {
      return false
    }
  }
  if (keyDirty) {
    const written = await writeKey()
    if (!written) {
      return false
    }
    if (!configDirty) {
      pushToast({ tone: 'success', message: '密钥已保存' })
    }
  }
  return true
}

function onReset(): void {
  apiKey.value = ''
  emit('reset')
}

async function testConnection(): Promise<void> {
  if (!canTest.value) {
    return
  }
  testing.value = true
  cardError.value = ''
  try {
    if (props.dirty || extraDirty.value) {
      const saved = await onSave()
      if (!saved) {
        cardError.value = '配置未保存，未发起连接测试'
        return
      }
    }
    await setupApi.testLlmConnection()
    pushToast({ tone: 'success', message: '连接测试通过' })
  } catch (cause) {
    cardError.value = errorMessage(cause, '连接测试未通过')
  } finally {
    testing.value = false
  }
}

onMounted(() => {
  void loadBackend()
})
</script>

<template>
  <SchemaGroupCard
    :group="props.group"
    :document="props.document"
    :dirty="props.dirty || extraDirty"
    :saving="props.saving || writing"
    :error="props.error || cardError"
    :reset-disabled="props.resetDisabled"
    :reset-hint="props.resetHint"
    @update="(field, value) => emit('update', field, value)"
    @save="onSave"
    @reset="onReset"
  >
    <section class="extra">
      <BaseInput
        v-model="apiKey"
        label="API Key"
        type="password"
        :disabled="!isOnline"
        :placeholder="keyPlaceholder"
        :hint="isOnline ? '密钥只写入系统凭据管理器，随本卡保存生效。' : '仅线上 provider 需要密钥。'"
      />

      <div class="kv-grid">
        <div class="kv-cell">
          <span class="kv-label">密钥</span>
          <b class="kv-value">{{ keyText }}</b>
        </div>
        <div class="kv-cell">
          <span class="kv-label">接入能力</span>
          <b class="kv-value">{{ capabilityText }}</b>
        </div>
        <div class="kv-cell">
          <span class="kv-label">上下文 / 输出</span>
          <b class="kv-value">{{ windowText }}</b>
        </div>
      </div>
    </section>

    <template #footExtra>
      <BaseButton
        variant="ghost"
        size="sm"
        :disabled="!canTest"
        :loading="testing"
        @click="testConnection"
      >
        测试连接
      </BaseButton>
    </template>
  </SchemaGroupCard>
</template>

<style scoped>
.extra {
  padding-top: var(--space-10);
  border-top: 1px dashed var(--border);
}

.kv-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(128px, 1fr));
  gap: var(--space-10) var(--space-14);
  margin-top: var(--space-10);
}

.kv-cell {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
}

.kv-label {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.kv-value {
  overflow: hidden;
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
  line-height: 1.5;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
