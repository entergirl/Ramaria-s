<script setup lang="ts">
/**
 * src/views/setup/StepModel.vue — 步骤 1：对话模型
 *
 * 职责:
 * - 收集运行方式、服务地址、模型名称与 API Key，并提供连接测试
 *
 * 设计特点:
 * - 运行方式是两张可选卡片（单选组），选中态用品牌浅底与实心圆点
 * - 本地 / 线上两组字段互斥显示，切换时由父组件清空测试结果
 * - 测试结果放在虚线状态条内，状态切换不推动下方内容
 */

import { computed, ref } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseInput from '../../components/base/BaseInput.vue'
import BaseSwitch from '../../components/base/BaseSwitch.vue'
import type { SetupFieldErrors, SetupMode } from '../../core/setupWizard'
import SetupStatusLine from './SetupStatusLine.vue'

const props = defineProps<{
  errors: SetupFieldErrors
  testing: boolean
  connectionState: 'idle' | 'ok' | 'fail'
  connectionMessage: string
}>()

const emit = defineEmits<{
  test: []
}>()

const mode = defineModel<SetupMode>('mode', { required: true })
const localUrl = defineModel<string>('localUrl', { required: true })
const localModel = defineModel<string>('localModel', { required: true })
const apiUrl = defineModel<string>('apiUrl', { required: true })
const apiKey = defineModel<string>('apiKey', { required: true })
const apiModel = defineModel<string>('apiModel', { required: true })
const vision = defineModel<boolean>('vision', { required: true })

const MODE_OPTIONS: { value: SetupMode; label: string; desc: string }[] = [
  {
    value: 'local',
    label: '本地部署',
    desc: 'LM Studio / Ollama 等本地推理服务，数据不出本机。',
  },
  {
    value: 'online',
    label: '线上 API',
    desc: 'DeepSeek / OpenAI 等，需要 API Key 与隐私确认。',
  },
]

const grid = ref<HTMLElement | null>(null)

const isLocal = computed(() => mode.value === 'local')

const statusTone = computed<'idle' | 'busy' | 'ok' | 'fail'>(() => {
  if (props.testing) {
    return 'busy'
  }
  return props.connectionState
})

const statusText = computed(() => {
  if (props.testing) {
    return '正在测试连接…'
  }
  if (props.connectionState === 'idle') {
    return '尚未测试连接'
  }
  return props.connectionMessage
})

function selectMode(value: string): void {
  mode.value = value === 'online' ? 'online' : 'local'
}

/** 单选组的左右 / 上下方向键：移动选中项并把焦点带到新卡片 */
function onModeKeydown(event: KeyboardEvent, index: number): void {
  if (
    event.key !== 'ArrowLeft' &&
    event.key !== 'ArrowRight' &&
    event.key !== 'ArrowUp' &&
    event.key !== 'ArrowDown'
  ) {
    return
  }
  event.preventDefault()
  const step = event.key === 'ArrowLeft' || event.key === 'ArrowUp' ? -1 : 1
  const total = MODE_OPTIONS.length
  const next = (index + step + total) % total
  const option = MODE_OPTIONS[next]
  if (!option) {
    return
  }
  mode.value = option.value
  const buttons = grid.value?.querySelectorAll<HTMLButtonElement>('.mode-card')
  buttons?.[next]?.focus()
}
</script>

<template>
  <section class="panel">
    <div class="panel-head">
      <h2 class="panel-title">选择对话模型</h2>
      <p class="panel-sub">本地部署无需联网与密钥；线上 API 需要先完成隐私确认。</p>
    </div>

    <div ref="grid" class="mode-grid" role="radiogroup" aria-label="对话模型运行方式">
      <button
        v-for="(option, index) in MODE_OPTIONS"
        :key="option.value"
        type="button"
        class="mode-card"
        :class="{ 'is-active': mode === option.value }"
        role="radio"
        :aria-checked="mode === option.value ? 'true' : 'false'"
        :tabindex="mode === option.value ? 0 : -1"
        @click="selectMode(option.value)"
        @keydown="onModeKeydown($event, index)"
      >
        <span class="mode-title">
          <span class="mode-radio" aria-hidden="true" />
          {{ option.label }}
        </span>
        <span class="mode-desc">{{ option.desc }}</span>
      </button>
    </div>

    <div v-if="isLocal" class="field-grid">
      <BaseInput
        v-model="localUrl"
        label="推理服务地址"
        placeholder="http://127.0.0.1:1234/v1"
        :error="errors.localUrl"
      />
      <BaseInput
        v-model="localModel"
        label="模型名称"
        placeholder="例如 qwen3-8b-instruct"
        :error="errors.localModel"
      />
    </div>

    <div v-else class="field-grid">
      <BaseInput
        v-model="apiUrl"
        class="span-2"
        label="API 地址"
        placeholder="https://api.deepseek.com/v1"
        :error="errors.apiUrl"
      />
      <BaseInput
        v-model="apiKey"
        type="password"
        label="API Key"
        placeholder="sk-…"
        :error="errors.apiKey"
      />
      <BaseInput
        v-model="apiModel"
        label="模型名称"
        placeholder="deepseek-chat"
        :error="errors.apiModel"
      />
    </div>

    <div class="vision">
      <BaseSwitch v-model="vision" label="模型支持图片理解（可选）" />
    </div>

    <div class="inline-status">
      <SetupStatusLine class="status-slot" :tone="statusTone" :text="statusText" />
      <BaseButton size="sm" variant="ghost" :loading="testing" @click="emit('test')">
        测试连接
      </BaseButton>
    </div>
  </section>
</template>

<style scoped>
.panel {
  display: flex;
  flex-direction: column;
  gap: var(--space-16);
}

.panel-head {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
}

.panel-title {
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
  line-height: var(--lh-h3);
}

.panel-sub {
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.mode-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: var(--space-12);
}

.mode-card {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
  padding: var(--space-14) var(--space-16);
  border: 1px solid var(--border);
  border-radius: var(--radius-session);
  background: var(--bg-card);
  box-shadow: var(--shadow-xs);
  text-align: left;
  cursor: pointer;
  transition:
    border-color var(--dur-fast) var(--ease-out),
    background-color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out),
    transform var(--dur-fast) var(--ease-out);
}

.mode-card:hover {
  border-color: var(--brand-line);
  box-shadow: var(--shadow-sm);
  transform: translateY(-1px);
}

.mode-card.is-active {
  border-color: var(--brand-line);
  background: var(--brand-surface);
}

.mode-card:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.mode-title {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  color: var(--text-1);
  font-size: var(--fs-body);
  font-weight: var(--fw-semibold);
  line-height: var(--lh-body);
}

.mode-radio {
  position: relative;
  flex: none;
  width: 16px;
  height: 16px;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-full);
  background: var(--bg-card);
}

.mode-card.is-active .mode-radio {
  border-color: var(--brand);
}

.mode-card.is-active .mode-radio::after {
  position: absolute;
  inset: 3px;
  border-radius: var(--radius-full);
  background: var(--brand);
  content: '';
}

.mode-desc {
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.field-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: var(--space-12) var(--space-16);
}

.span-2 {
  grid-column: 1 / -1;
}

.vision {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
}

.inline-status {
  display: flex;
  align-items: center;
  gap: var(--space-10);
  padding: var(--space-10) var(--space-12);
  border: 1px dashed var(--border-strong);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.status-slot {
  flex: 1;
}

@media (max-width: 560px) {
  .mode-grid,
  .field-grid {
    grid-template-columns: 1fr;
  }
}
</style>
