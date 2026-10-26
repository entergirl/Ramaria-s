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
 * - 段数上限 5、选中段不换行；默认档选中白底 + 品牌描边 + 微小投影
 * - `plain` 档去掉容器背板，各段自成卡片键（用于记忆页页签）
 * - `lg` 档放大段高与左右内距，`pill` 档把容器与各段切成胶囊（用于设置页范围切换）
 * - 可选计数徽标用于带数量的页签，缺省不占位
 */

import { ref } from 'vue'

interface SegmentOption {
  value: string
  label: string
  /** 可选计数徽标（页签数量等），0 与缺省都不渲染 */
  count?: number
}

const props = withDefaults(
  defineProps<{
    modelValue: string
    options: SegmentOption[]
    label?: string
    disabled?: boolean
    /** default = 凹槽分段；plain = 无背板、各段独立成键 */
    variant?: 'default' | 'plain'
    /** md = 常规段高；lg = 放大段高与内距 */
    size?: 'md' | 'lg'
    /** 容器与各段切成胶囊 */
    pill?: boolean
  }>(),
  {
    label: '',
    disabled: false,
    variant: 'default',
    size: 'md',
    pill: false,
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
    :class="[
      `segmented--${variant}`,
      `segmented--${size}`,
      { 'segmented--pill': pill, 'is-disabled': disabled },
    ]"
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
      <span>{{ option.label }}</span>
      <span v-if="option.count" class="segmented-count">{{ option.count }}</span>
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
  display: inline-flex;
  align-items: center;
  gap: var(--space-6);
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

.segmented-count {
  min-width: 16px;
  padding: 0 var(--space-4);
  border-radius: var(--radius-tag);
  background: var(--bg-sunken);
  color: var(--text-3);
  font-size: var(--fs-caption);
  font-weight: var(--fw-semibold);
  line-height: 16px;
  text-align: center;
}

.segmented-item[aria-selected='true'] .segmented-count {
  background: var(--brand-surface);
  color: var(--brand-text);
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

/* ── plain：去掉容器背板，各段独立成键 ── */
.segmented--plain {
  gap: var(--space-8);
  padding: 0;
  background: transparent;
}

.segmented--plain .segmented-item {
  height: 30px;
  padding: 0 var(--space-14);
  border-color: var(--border);
  border-radius: var(--radius-btn-sm);
  background: var(--bg-card);
  color: var(--text-2);
  font-weight: var(--fw-semibold);
  box-shadow: var(--shadow-xs);
}

.segmented--plain .segmented-item:hover:not(:disabled):not([aria-selected='true']) {
  border-color: var(--brand-line);
  color: var(--brand-text);
}

.segmented--plain .segmented-item[aria-selected='true'] {
  background: var(--btn-bg);
  border-color: transparent;
  color: var(--btn-fg);
  box-shadow: var(--btn-shadow);
}

.segmented--plain .segmented-count {
  min-width: 0;
  padding: 0;
  border-radius: 0;
  background: transparent;
  color: var(--text-3);
  line-height: 1;
}

.segmented--plain .segmented-item[aria-selected='true'] .segmented-count {
  background: transparent;
  color: var(--btn-fg);
  opacity: 0.82;
}

.segmented--plain .segmented-item:focus-visible {
  box-shadow: var(--focus-ring);
}

/* ── lg：放大段高与内距 ── */
.segmented--lg {
  gap: var(--space-6);
  padding: var(--space-4);
}

.segmented--lg .segmented-item {
  height: 30px;
  padding: 0 var(--space-20);
  font-size: var(--fs-body);
  font-weight: var(--fw-semibold);
}

.segmented--lg .segmented-count {
  min-width: 18px;
  line-height: 18px;
}

/* ── pill：容器与各段切成胶囊 ── */
.segmented--pill {
  border-radius: var(--radius-pill);
}

.segmented--pill .segmented-item {
  border-radius: var(--radius-pill);
}
</style>
