<script setup lang="ts">
/**
 * src/views/settings/custom/McpCard.vue — MCP 接入卡
 *
 * 职责:
 * - 在 MCP 配置键之外补充运行信息、人格白名单选项与客户端配置片段
 *
 * 设计特点:
 * - 白名单选项来自人格列表（运行时注入），键表只声明字段与默认口径
 * - 配置片段由命令返回值拼装，复制即用；不猜测安装路径
 * - 通道活动统计失败不影响配置读写（面板信息降级为默认值）
 */

import { computed, onMounted, ref } from 'vue'

import BaseButton from '../../../components/base/BaseButton.vue'
import BaseTag from '../../../components/base/BaseTag.vue'
import { mcpApi, personaApi } from '../../../core/api'
import type { McpInfo, PersonaFull } from '../../../core/api/dto'
import type { JsonValue } from '../../../config/configTypes'
import type { SettingField, SettingGroup, SettingOption } from '../../../config/schema/define'
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
  save: []
  reset: []
}>()

const info = ref<McpInfo | null>(null)
const personas = ref<PersonaFull[]>([])
const loading = ref(false)
const cardError = ref('')
const copied = ref('')

const optionOverrides = computed<Record<string, SettingOption[]>>(() => ({
  'mcp.allowed_personas': [
    { value: '*', label: '全部人格' },
    ...personas.value.map((persona) => ({ value: persona.uid, label: persona.name })),
  ],
}))

const combinedError = computed(() => props.error || cardError.value)

const snippet = computed(() => {
  if (!info.value) {
    return ''
  }
  const config = {
    mcpServers: {
      ramaria: {
        command: info.value.command,
        args: ['mcp', 'serve'],
      },
    },
  }
  return JSON.stringify(config, null, 2)
})

const activityText = computed(() => {
  if (!info.value) {
    return '读取中…'
  }
  if (!info.value.lastActivityMs) {
    return '暂无活动'
  }
  const minutes = Math.max(1, Math.round((Date.now() - info.value.lastActivityMs) / 60000))
  return minutes < 60 ? `${minutes} 分钟前` : `${Math.round(minutes / 60)} 小时前`
})

async function loadInfo(): Promise<void> {
  if (loading.value) {
    return
  }
  loading.value = true
  cardError.value = ''
  try {
    const [nextInfo, nextPersonas] = await Promise.all([mcpApi.getInfo(), personaApi.listFull()])
    info.value = nextInfo
    personas.value = nextPersonas
  } catch (cause) {
    cardError.value = errorMessage(cause, 'MCP 信息读取失败')
  } finally {
    loading.value = false
  }
}

async function copyText(text: string, label: string): Promise<void> {
  if (!text) {
    return
  }
  try {
    await navigator.clipboard.writeText(text)
    copied.value = label
    pushToast({ tone: 'success', message: `${label}已复制` })
  } catch (cause) {
    pushToast({ tone: 'danger', message: errorMessage(cause, '复制失败，请手动选择文本') })
  }
}

onMounted(() => {
  void loadInfo()
})
</script>

<template>
  <SchemaGroupCard
    :group="props.group"
    :document="props.document"
    :dirty="props.dirty"
    :saving="props.saving"
    :error="combinedError"
    :reset-disabled="props.resetDisabled"
    :reset-hint="props.resetHint"
    :option-overrides="optionOverrides"
    @update="(field, value) => emit('update', field, value)"
    @save="emit('save')"
    @reset="emit('reset')"
  >
    <template #head>
      <BaseTag :tone="info?.enabled ? 'ok' : 'neutral'">
        {{ info?.enabled ? '已启用' : '未启用' }}
      </BaseTag>
    </template>

    <section class="extra">
      <h4 class="extra-title">运行信息</h4>
      <div class="row">
        <span class="row-label">最近活动</span>
        <span class="row-value">{{ activityText }}</span>
      </div>
      <div class="row">
        <span class="row-label">活跃会话</span>
        <span class="row-value">{{ info?.activeSessions ?? 0 }}</span>
      </div>
      <div class="row">
        <span class="row-label">数据库</span>
        <code class="row-code">{{ info?.dbPath ?? '读取中…' }}</code>
        <BaseButton
          variant="ghost"
          size="sm"
          :disabled="!info"
          @click="copyText(info?.dbPath ?? '', '数据库路径')"
        >
          复制
        </BaseButton>
      </div>
      <p v-if="info && !info.commandIsBundled" class="extra-note">
        CLI 未随应用打包：需将安装目录加入 PATH，客户端才能以 `ramaria` 启动服务。
      </p>
    </section>

    <section class="extra">
      <div class="snippet-head">
        <h4 class="extra-title">客户端配置片段</h4>
        <BaseButton variant="ghost" size="sm" :disabled="!snippet" @click="copyText(snippet, '配置片段')">
          {{ copied === '配置片段' ? '已复制' : '复制' }}
        </BaseButton>
      </div>
      <pre class="snippet">{{ snippet || '读取中…' }}</pre>
    </section>

    <template #footExtra>
      <BaseButton variant="ghost" size="sm" :loading="loading" @click="loadInfo">刷新信息</BaseButton>
    </template>
  </SchemaGroupCard>
</template>

<style scoped>
.extra {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
  padding-top: var(--space-10);
  border-top: 1px dashed var(--border);
}

.extra-title {
  margin: 0;
  color: var(--text-2);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
}

.extra-note {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
}

.row {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  font-size: var(--fs-small);
}

.row-label {
  flex: none;
  width: 64px;
  color: var(--text-3);
}

.row-value {
  flex: 1;
  min-width: 0;
  color: var(--text-1);
}

.row-code {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  color: var(--text-2);
  font-family: var(--font-mono);
  font-size: var(--fs-code);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.snippet-head {
  display: flex;
  align-items: center;
  gap: var(--space-8);
}

.snippet-head .extra-title {
  flex: 1;
}

.snippet {
  margin: 0;
  padding: var(--space-10);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-sunken);
  color: var(--text-2);
  font-family: var(--font-mono);
  font-size: var(--fs-code);
  line-height: 1.6;
  overflow-x: auto;
  white-space: pre;
}
</style>
