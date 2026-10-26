<script setup lang="ts">
/**
 * src/components/base/BaseChipNav.vue — 胶囊分类键条
 *
 * 职责:
 * - 在若干互斥分类间切换：浅底胶囊背板承托一排胶囊键，选中键深底浅字
 * - 键上可挂数量：默认行内计数，`badge` 档改以警示色徽标呈现
 *
 * 设计特点:
 * - 容器为 tablist、键为 tab，选中态走 aria-selected；支持逐键 aria-label 覆盖
 * - 左右方向键在键间移动并同步选中，焦点跟随
 * - 键数不限，窄窗在背板内换行，背板圆角回落为矩形圆角；单键尺寸与换行无关
 * - 与分段控件的分工：分段控件限 2~5 段、表达分档；本组件表达分类导航，段数不限
 */

import { ref } from 'vue'

/** 单个分类键 */
interface ChipNavOption {
  value: string
  label: string
  /** 可选计数；0 与缺省都不渲染 */
  count?: number
  /** 读屏用名称，缺省取 label */
  ariaLabel?: string
}

const props = withDefaults(
  defineProps<{
    modelValue: string
    options: ChipNavOption[]
    label?: string
    /** 计数改以警示徽标呈现（用于未保存项） */
    badge?: boolean
    disabled?: boolean
  }>(),
  {
    label: '',
    badge: false,
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
    class="chip-nav"
    :class="{ 'chip-nav--badge': badge, 'is-disabled': disabled }"
    role="tablist"
    :aria-label="label || undefined"
  >
    <button
      v-for="(option, index) in options"
      :key="option.value"
      type="button"
      role="tab"
      class="chip-nav-item"
      :aria-selected="option.value === modelValue ? 'true' : 'false'"
      :aria-label="option.ariaLabel || undefined"
      :disabled="disabled"
      @click="select(option.value)"
      @keydown="onKeydown($event, index)"
    >
      <span>{{ option.label }}</span>
      <span v-if="option.count" class="chip-nav-count">{{ option.count }}</span>
    </button>
  </div>
</template>

<style scoped>
.chip-nav {
  display: inline-flex;
  flex-wrap: wrap;
  justify-content: center;
  gap: var(--space-4);
  max-width: 100%;
  padding: var(--space-4);
  border: 1px solid var(--border);
  border-radius: var(--radius-pill);
  background: var(--bg-subtle);
  box-shadow: var(--shadow-xs);
  white-space: nowrap;
}

.chip-nav-item {
  display: inline-flex;
  flex: none;
  align-items: center;
  gap: var(--space-6);
  height: 32px;
  padding: 0 var(--space-14);
  border: 1px solid transparent;
  border-radius: var(--radius-pill);
  background: transparent;
  color: var(--text-2);
  font-size: var(--fs-body);
  font-weight: var(--fw-semibold);
  cursor: pointer;
  transition:
    background-color var(--dur-fast) var(--ease-out),
    border-color var(--dur-fast) var(--ease-out),
    color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out);
}

.chip-nav-item:hover:not(:disabled):not([aria-selected='true']) {
  background: var(--bg-card);
  color: var(--brand-text);
}

.chip-nav-item[aria-selected='true'] {
  background: var(--btn-bg);
  border-color: transparent;
  color: var(--btn-fg);
  box-shadow: var(--btn-shadow);
}

.chip-nav-item:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

/* ── 行内计数：随选中态同色降透明度 ── */
.chip-nav-count {
  color: var(--text-3);
  font-size: var(--fs-caption);
  font-weight: var(--fw-bold);
  line-height: 1;
}

.chip-nav-item[aria-selected='true'] .chip-nav-count {
  color: var(--btn-fg);
  opacity: 0.82;
}

/* ── 徽标档：警示色白底，选中与否都保持可辨识 ── */
.chip-nav--badge .chip-nav-count {
  min-width: 17px;
  height: 17px;
  padding: 0 var(--space-4);
  border: 1px solid var(--warn-line);
  border-radius: var(--radius-pill);
  background: var(--bg-card);
  color: var(--warn);
  font-size: var(--fs-caption);
  font-weight: var(--fw-bold);
  line-height: 15px;
  text-align: center;
}

.chip-nav--badge .chip-nav-item[aria-selected='true'] .chip-nav-count {
  color: var(--warn);
  opacity: 1;
}

.chip-nav.is-disabled {
  opacity: var(--opacity-disabled);
}

.chip-nav-item:disabled {
  cursor: default;
}

/* 换行后胶囊端头失去意义，背板回落为矩形圆角 */
@media (max-width: 720px) {
  .chip-nav {
    border-radius: var(--radius-inner);
  }
}
</style>
