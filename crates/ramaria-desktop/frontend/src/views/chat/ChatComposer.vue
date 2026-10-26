<script setup lang="ts">
/**
 * src/views/chat/ChatComposer.vue — 消息输入区
 *
 * 职责:
 * - 承载文本输入、发送键与发图键
 * - 处理 Enter 发送、Shift+Enter 换行与中文输入法合成期保护
 * - 流式与发送中锁定输入，锁定态给出占位说明
 *
 * 设计特点:
 * - 输入法合成期间不触发发送，避免选词回车误发
 * - 锁定用原生 disabled 表达，不改变控件高度
 * - 发图键以浅底与白色输入卡拉开色差；选择文件期间只禁用，不显示旋转指示
 * - 发图键只在可用时响应，失败提示由上层处理
 */
import { computed, ref } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'

const props = withDefaults(
  defineProps<{
    modelValue: string
    disabled?: boolean
    locked?: boolean
    attaching?: boolean
  }>(),
  { disabled: false, locked: false, attaching: false },
)

const emit = defineEmits<{
  'update:modelValue': [value: string]
  send: [text: string]
  attach: []
}>()

const composing = ref(false)

const placeholder = computed(() => {
  if (props.locked) {
    return '正在生成…'
  }
  if (props.disabled) {
    return '当前不可输入'
  }
  return '说点什么…（Enter 发送，Shift+Enter 换行）'
})

const canSend = computed(
  () => !props.disabled && !props.locked && props.modelValue.trim() !== '',
)

function submit(): void {
  if (!canSend.value) {
    return
  }
  const text = props.modelValue.trim()
  if (!text) {
    return
  }
  emit('send', text)
  emit('update:modelValue', '')
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Enter' || event.shiftKey) {
    return
  }
  if (composing.value || event.isComposing) {
    return
  }
  event.preventDefault()
  submit()
}
</script>

<template>
  <section class="composer">
    <BaseButton
      variant="secondary"
      class="attach-btn"
      icon-only
      aria-label="发送图片"
      title="发送图片"
      :disabled="disabled || locked || attaching"
      @click="emit('attach')"
    >
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <rect x="3" y="5" width="18" height="14" rx="2" />
        <circle cx="9" cy="10" r="1.5" />
        <path d="m21 15-5-5L5 21" />
      </svg>
    </BaseButton>

    <textarea
      class="composer-input"
      rows="1"
      :value="modelValue"
      :placeholder="placeholder"
      :disabled="disabled || locked"
      aria-label="消息输入"
      @input="emit('update:modelValue', ($event.target as HTMLTextAreaElement).value)"
      @keydown="onKeydown"
      @compositionstart="composing = true"
      @compositionend="composing = false"
    />

    <BaseButton
      variant="primary"
      icon-only
      aria-label="发送"
      title="发送"
      :disabled="!canSend"
      @click="submit"
    >
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <line x1="12" y1="19" x2="12" y2="5" />
        <polyline points="5 12 12 5 19 12" />
      </svg>
    </BaseButton>
  </section>
</template>

<style scoped>
.composer {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--space-10);
  min-height: var(--composer-h);
  padding: var(--space-8) var(--space-8) var(--space-8) var(--space-12);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-md);
}

/* 发图键与白色输入卡拉开色差，悬停回到品牌浅底 */
.composer .attach-btn {
  background: var(--bg-subtle);
  border-color: var(--border);
  color: var(--text-2);
}

.composer .attach-btn:hover:not(:disabled) {
  background: var(--brand-surface);
  border-color: var(--brand-line);
  color: var(--brand-text);
}

.composer-input {
  flex: 1;
  min-width: 0;
  height: calc(var(--composer-h) - var(--space-16));
  max-height: calc(var(--composer-h) - var(--space-16));
  padding: var(--space-10) var(--space-4);
  border: none;
  background: transparent;
  color: var(--text-1);
  font-family: inherit;
  font-size: var(--fs-body);
  line-height: var(--lh-body);
  resize: none;
}

.composer-input::placeholder {
  color: var(--text-3);
}

.composer-input:focus-visible {
  outline: none;
}

.composer-input:disabled {
  color: var(--text-3);
}

.composer :deep(.btn) {
  flex: none;
}

.composer svg {
  width: 17px;
  height: 17px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.9;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.composer :deep(.btn--primary) svg {
  stroke-width: 2.2;
}
</style>
