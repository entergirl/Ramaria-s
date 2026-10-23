<script setup lang="ts">
/**
 * src/components/base/BaseSwitch.vue — 开关
 *
 * 职责:
 * - 提供开 / 关切换与写入中锁定、行内保存指示
 *
 * 设计特点:
 * - 原生 checkbox 承载状态与可访问性，样式层只画轨道与滑块
 * - 写入中锁定输入并给行内指示，失败回滚由调用方处理
 * - 标签随控件整体可点，命中区域不小于最小可点尺寸
 */

withDefaults(
  defineProps<{
    modelValue: boolean
    label?: string
    saving?: boolean
    disabled?: boolean
  }>(),
  {
    label: '',
    saving: false,
    disabled: false,
  },
)

const emit = defineEmits<{
  'update:modelValue': [value: boolean]
}>()

function onChange(event: Event): void {
  const target = event.target as HTMLInputElement
  emit('update:modelValue', target.checked)
}
</script>

<template>
  <label class="switch" :class="{ 'is-disabled': disabled || saving }">
    <input
      class="switch-input"
      type="checkbox"
      role="switch"
      :checked="modelValue"
      :disabled="disabled || saving"
      @change="onChange"
    />
    <span class="switch-track" aria-hidden="true">
      <span class="switch-thumb" />
    </span>
    <span v-if="label" class="switch-label">{{ label }}</span>
    <span v-if="saving" class="switch-saving" role="status">保存中</span>
  </label>
</template>

<style scoped>
.switch {
  position: relative;
  display: inline-flex;
  align-items: center;
  gap: var(--space-8);
  min-height: var(--hit-size);
  cursor: pointer;
}

.switch.is-disabled {
  cursor: default;
}

.switch-input {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  margin: 0;
  opacity: 0;
  cursor: inherit;
}

.switch-track {
  position: relative;
  flex: none;
  width: 36px;
  height: 20px;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-full);
  background: var(--bg-sunken);
  transition:
    background-color var(--dur-fast) var(--ease-out),
    border-color var(--dur-fast) var(--ease-out);
}

.switch-thumb {
  position: absolute;
  top: 1px;
  left: 1px;
  width: 16px;
  height: 16px;
  border-radius: var(--radius-full);
  background: var(--bg-card);
  box-shadow: var(--shadow-xs);
  transition: transform var(--dur-fast) var(--ease-out);
}

.switch-input:checked + .switch-track {
  background: var(--brand);
  border-color: var(--brand);
}

.switch-input:checked + .switch-track .switch-thumb {
  transform: translateX(16px);
}

.switch:hover .switch-input:not(:disabled) + .switch-track {
  border-color: var(--brand-line);
}

.switch-input:focus-visible + .switch-track {
  box-shadow: var(--focus-ring);
}

.switch-input:disabled + .switch-track {
  opacity: var(--opacity-disabled);
}

.switch-label {
  color: var(--text-1);
  font-size: var(--fs-body);
}

.switch.is-disabled .switch-label {
  color: var(--text-3);
}

.switch-saving {
  color: var(--text-3);
  font-size: var(--fs-small);
}
</style>
