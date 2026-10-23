<script setup lang="ts">
/**
 * src/components/base/BaseSegmented.vue — 分段控件
 *
 * 职责:
 * - 在 2~5 个互斥选项间切换，表达页签或视图内分档
 *
 * 设计特点:
 * - 容器为 tablist，选项为 tab，选中态走 aria-selected
 * - 左右方向键在段间移动并同步选中，焦点跟随
 * - 段数上限 5、选中段不换行；选中白底 + 品牌描边 + 微小投影
 */

import { ref } from 'vue'

interface SegmentOption {
  value: string
  label: string
}

const props = withDefaults(
  defineProps<{
    modelValue: string
    options: SegmentOption[]
    label?: string
    disabled?: boolean
  }>(),
  {
    label: '',
    disabled: false,
  },
)

const emit = defineEmits<{
  'update:modelValue': [value: string]
}>()

const group = ref<HTMLElement | null>(null)

function select(value: string): void {
  if (props.disabled) {
    return
  }
  emit('update:modelValue', value)
}

function onKeydown(event: KeyboardEvent, index: number): void {
  if (props.disabled || (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight')) {
    return
  }
  event.preventDefault()
  const total = props.options.length
  const step = event.key === 'ArrowRight' ? 1 : -1
  const next = (index + step + total) % total
  const option = props.options[next]
  if (!option) {
    return
  }
  select(option.value)
  const buttons = group.value?.querySelectorAll('button')
  buttons?.[next]?.focus()
}
</script>

<template>
  <div
    ref="group"
    class="segmented"
    :class="{ 'is-disabled': disabled }"
    role="tablist"
    :aria-label="label || undefined"
  >
    <button
      v-for="(option, index) in options"
      :key="option.value"
      type="button"
      role="tab"
      class="segmented-item"
      :aria-selected="option.value === modelValue ? 'true' : 'false'"
      :disabled="disabled"
      @click="select(option.value)"
      @keydown="onKeydown($event, index)"
    >
      {{ option.label }}
    </button>
  </div>
</template>

<style scoped>
.segmented {
  display: inline-flex;
  gap: var(--space-4);
  padding: 3px;
  border-radius: var(--radius-btn);
  background: var(--bg-subtle);
  white-space: nowrap;
}

.segmented-item {
  height: 26px;
  padding: 0 var(--space-12);
  border: 1px solid transparent;
  border-radius: var(--radius-tag);
  background: transparent;
  color: var(--text-2);
  font-size: var(--fs-small);
  font-weight: var(--fw-medium);
  cursor: pointer;
  transition:
    background-color var(--dur-fast) var(--ease-out),
    color var(--dur-fast) var(--ease-out),
    border-color var(--dur-fast) var(--ease-out);
}

.segmented-item:hover:not(:disabled) {
  color: var(--text-1);
}

.segmented-item[aria-selected='true'] {
  background: var(--bg-card);
  border-color: var(--brand-line);
  color: var(--brand-text);
  box-shadow: var(--shadow-xs);
}

.segmented-item:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.segmented.is-disabled {
  opacity: var(--opacity-disabled);
}

.segmented-item:disabled {
  cursor: default;
}
</style>
