<script setup lang="ts">
/**
 * src/components/base/BaseRadio.vue — 单选项
 *
 * 职责:
 * - 提供单个单选项；同 `name` 的多个实例构成原生单选组
 *
 * 设计特点:
 * - 组内方向键切换由原生 radio 分组承担，不在此重复实现
 * - 标签整体可点，选中环用品牌色，错误态只描边不叠灰
 */

const props = withDefaults(
  defineProps<{
    modelValue: string
    value: string
    name: string
    label: string
    disabled?: boolean
    invalid?: boolean
  }>(),
  {
    disabled: false,
    invalid: false,
  },
)

const emit = defineEmits<{
  'update:modelValue': [value: string]
}>()

function onChange(): void {
  emit('update:modelValue', props.value)
}
</script>

<template>
  <label class="radio" :class="{ 'is-disabled': disabled }">
    <input
      class="radio-input"
      type="radio"
      :name="name"
      :value="value"
      :checked="modelValue === value"
      :disabled="disabled"
      :aria-invalid="invalid ? 'true' : undefined"
      @change="onChange"
    />
    <span class="radio-dot" aria-hidden="true" />
    <span class="radio-label">{{ label }}</span>
  </label>
</template>

<style scoped>
.radio {
  position: relative;
  display: inline-flex;
  align-items: center;
  gap: var(--space-8);
  min-height: var(--hit-size);
  cursor: pointer;
}

.radio.is-disabled {
  cursor: default;
}

.radio-input {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  margin: 0;
  opacity: 0;
  cursor: inherit;
}

.radio-dot {
  position: relative;
  flex: none;
  width: 16px;
  height: 16px;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-full);
  background: var(--bg-card);
  transition:
    border-color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out);
}

.radio-input:checked + .radio-dot {
  border-color: var(--brand);
  box-shadow: inset 0 0 0 3px var(--bg-card), inset 0 0 0 8px var(--brand);
}

.radio:hover .radio-input:not(:disabled) + .radio-dot {
  border-color: var(--brand-line);
}

.radio-input:focus-visible + .radio-dot {
  box-shadow: var(--focus-ring);
}

.radio-input[aria-invalid='true'] + .radio-dot {
  border-color: var(--danger);
}

.radio-input:disabled + .radio-dot {
  opacity: var(--opacity-disabled);
}

.radio-label {
  color: var(--text-1);
  font-size: var(--fs-body);
}

.radio.is-disabled .radio-label {
  color: var(--text-3);
}
</style>
