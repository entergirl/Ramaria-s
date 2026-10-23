<script setup lang="ts">
/**
 * src/components/base/BaseCheckbox.vue — 复选框
 *
 * 职责:
 * - 提供未选 / 已选 / 半选三态与禁用、无效表达
 *
 * 设计特点:
 * - 原生 checkbox 承载状态，半选经 `indeterminate` 属性同步
 * - 标签整体可点，命中区域不小于最小可点尺寸
 * - 勾选标记用内联 SVG 描边，不依赖字体图标
 */

import { ref, watchEffect } from 'vue'

const props = withDefaults(
  defineProps<{
    modelValue: boolean
    label: string
    indeterminate?: boolean
    disabled?: boolean
    invalid?: boolean
  }>(),
  {
    indeterminate: false,
    disabled: false,
    invalid: false,
  },
)

const emit = defineEmits<{
  'update:modelValue': [value: boolean]
}>()

const input = ref<HTMLInputElement | null>(null)

watchEffect(() => {
  if (input.value) {
    input.value.indeterminate = props.indeterminate
  }
})

function onChange(event: Event): void {
  const target = event.target as HTMLInputElement
  emit('update:modelValue', target.checked)
}
</script>

<template>
  <label class="checkbox" :class="{ 'is-disabled': disabled }">
    <input
      ref="input"
      class="checkbox-input"
      type="checkbox"
      :checked="modelValue"
      :disabled="disabled"
      :aria-invalid="invalid ? 'true' : undefined"
      @change="onChange"
    />
    <span class="checkbox-box" aria-hidden="true">
      <svg v-if="indeterminate" class="checkbox-mark" viewBox="0 0 12 12">
        <path d="M3 6h6" />
      </svg>
      <svg v-else-if="modelValue" class="checkbox-mark" viewBox="0 0 12 12">
        <path d="M2.5 6.2 4.8 8.5 9.5 3.8" />
      </svg>
    </span>
    <span class="checkbox-label">{{ label }}</span>
  </label>
</template>

<style scoped>
.checkbox {
  position: relative;
  display: inline-flex;
  align-items: center;
  gap: var(--space-8);
  min-height: var(--hit-size);
  cursor: pointer;
}

.checkbox.is-disabled {
  cursor: default;
}

.checkbox-input {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  margin: 0;
  opacity: 0;
  cursor: inherit;
}

.checkbox-box {
  display: grid;
  place-items: center;
  flex: none;
  width: 16px;
  height: 16px;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-check);
  background: var(--bg-card);
  transition:
    background-color var(--dur-fast) var(--ease-out),
    border-color var(--dur-fast) var(--ease-out);
}

.checkbox-mark {
  width: 12px;
  height: 12px;
  fill: none;
  stroke: var(--btn-fg);
  stroke-width: 1.8;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.checkbox-input:checked + .checkbox-box,
.checkbox-input:indeterminate + .checkbox-box {
  background: var(--brand);
  border-color: var(--brand);
}

.checkbox:hover .checkbox-input:not(:disabled) + .checkbox-box {
  border-color: var(--brand-line);
}

.checkbox-input:focus-visible + .checkbox-box {
  box-shadow: var(--focus-ring);
}

.checkbox-input[aria-invalid='true'] + .checkbox-box {
  border-color: var(--danger);
}

.checkbox-input:disabled + .checkbox-box {
  opacity: var(--opacity-disabled);
}

.checkbox-label {
  color: var(--text-1);
  font-size: var(--fs-body);
}

.checkbox.is-disabled .checkbox-label {
  color: var(--text-3);
}
</style>
