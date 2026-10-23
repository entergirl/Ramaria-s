<script setup lang="ts">
/**
 * src/components/base/BaseInput.vue — 单行输入
 *
 * 职责:
 * - 提供外置标签、提示 / 错误行与校验中指示
 *
 * 设计特点:
 * - 标签外置，不复用 placeholder 充当字段名
 * - 提示行固定占位，出现错误不改变控件高度
 * - 只读数据用静态文本呈现，本组件不提供 readonly 形态
 */

import { useId } from 'vue'

const props = withDefaults(
  defineProps<{
    modelValue: string
    label: string
    type?: string
    placeholder?: string
    hint?: string
    error?: string
    invalid?: boolean
    validating?: boolean
    disabled?: boolean
    id?: string
  }>(),
  {
    type: 'text',
    placeholder: '',
    hint: '',
    error: '',
    invalid: false,
    validating: false,
    disabled: false,
    id: undefined,
  },
)

const emit = defineEmits<{
  'update:modelValue': [value: string]
}>()

const fieldId = props.id ?? useId()
const messageId = `${fieldId}-message`

function onInput(event: Event): void {
  const target = event.target as HTMLInputElement
  emit('update:modelValue', target.value)
}
</script>

<template>
  <div class="field" :data-validating="validating ? '' : undefined">
    <label class="field-label" :for="fieldId">{{ label }}</label>
    <div class="field-control">
      <input
        :id="fieldId"
        class="field-input"
        :type="type"
        :value="modelValue"
        :placeholder="placeholder"
        :disabled="disabled"
        :aria-invalid="invalid || error ? 'true' : undefined"
        :aria-describedby="error || hint ? messageId : undefined"
        @input="onInput"
      />
      <span v-if="validating" class="field-spinner" aria-hidden="true" />
    </div>
    <p :id="messageId" class="field-message" :class="{ 'is-error': !!error }">
      {{ error || hint }}
    </p>
  </div>
</template>

<style scoped>
.field {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
}

.field-label {
  color: var(--text-2);
  font-size: var(--fs-small);
  font-weight: var(--fw-medium);
}

.field-control {
  position: relative;
  display: flex;
  align-items: center;
}

.field-input {
  width: 100%;
  height: 32px;
  padding: 0 var(--space-10);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-input);
  background: var(--bg-subtle);
  color: var(--text-1);
  font-family: inherit;
  font-size: var(--fs-body);
  transition:
    border-color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out);
}

.field-input::placeholder {
  color: var(--text-3);
}

.field-input:hover:not(:disabled) {
  border-color: var(--brand-line);
}

.field-input:focus-visible {
  outline: none;
  border-color: var(--brand);
  box-shadow: var(--focus-ring);
}

.field-input[aria-invalid='true'] {
  border-color: var(--danger);
}

.field-input[aria-invalid='true']:focus-visible {
  box-shadow: 0 0 0 3px var(--danger-line);
}

.field-input:disabled {
  opacity: var(--opacity-disabled);
  cursor: default;
}

.field-spinner {
  position: absolute;
  right: var(--space-10);
  width: 12px;
  height: 12px;
  border: 2px solid var(--text-3);
  border-right-color: transparent;
  border-radius: var(--radius-full);
  animation: field-spin 700ms linear infinite;
}

.field-message {
  min-height: 18px;
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: 1.5;
}

.field-message.is-error {
  color: var(--danger);
}

@keyframes field-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (prefers-reduced-motion: reduce) {
  .field-spinner {
    animation: none;
  }
}
</style>
