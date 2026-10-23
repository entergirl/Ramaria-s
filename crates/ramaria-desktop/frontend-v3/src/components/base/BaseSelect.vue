<script setup lang="ts">
/**
 * src/components/base/BaseSelect.vue — 下拉选择
 *
 * 职责:
 * - 提供触发展开、选项列表、键盘选择与空选项说明
 *
 * 设计特点:
 * - 触发键为 combobox 语义（aria-expanded / aria-controls），列表为 listbox
 * - 上下键移动高亮、Enter 选中、Esc 收起；点击组件外收起
 * - 选项为空时触发键显示「暂无可选项」且不可展开
 */

import { computed, onBeforeUnmount, onMounted, ref, useId } from 'vue'

interface SelectOption {
  value: string
  label: string
}

const props = withDefaults(
  defineProps<{
    modelValue: string
    label: string
    options: SelectOption[]
    placeholder?: string
    hint?: string
    error?: string
    invalid?: boolean
    disabled?: boolean
    id?: string
  }>(),
  {
    placeholder: '请选择',
    hint: '',
    error: '',
    invalid: false,
    disabled: false,
    id: undefined,
  },
)

const emit = defineEmits<{
  'update:modelValue': [value: string]
}>()

const fieldId = props.id ?? useId()
const labelId = `${fieldId}-label`
const listId = `${fieldId}-listbox`
const messageId = `${fieldId}-message`

const root = ref<HTMLElement | null>(null)
const trigger = ref<HTMLButtonElement | null>(null)
const open = ref(false)
const activeIndex = ref(-1)

const hasOptions = computed(() => props.options.length > 0)

const selectedLabel = computed(() => {
  const hit = props.options.find((option) => option.value === props.modelValue)
  return hit ? hit.label : ''
})

function toggle(): void {
  if (props.disabled || !hasOptions.value) {
    return
  }
  if (open.value) {
    close()
    return
  }
  open.value = true
  const index = props.options.findIndex((option) => option.value === props.modelValue)
  activeIndex.value = index >= 0 ? index : 0
}

function close(): void {
  open.value = false
  activeIndex.value = -1
}

function select(option: SelectOption): void {
  emit('update:modelValue', option.value)
  close()
  trigger.value?.focus()
}

function move(step: number): void {
  if (!open.value) {
    open.value = true
    activeIndex.value = 0
    return
  }
  const total = props.options.length
  activeIndex.value = (activeIndex.value + step + total) % total
}

function onKeydown(event: KeyboardEvent): void {
  if (props.disabled) {
    return
  }
  if (event.key === 'Escape') {
    if (open.value) {
      event.stopPropagation()
      close()
    }
    return
  }
  if (event.key === 'ArrowDown') {
    event.preventDefault()
    move(1)
    return
  }
  if (event.key === 'ArrowUp') {
    event.preventDefault()
    move(-1)
    return
  }
  if ((event.key === 'Enter' || event.key === ' ') && open.value) {
    event.preventDefault()
    const option = props.options[activeIndex.value]
    if (option) {
      select(option)
    }
  }
}

function onDocumentPointerDown(event: PointerEvent): void {
  if (!open.value) {
    return
  }
  const target = event.target as Node | null
  if (target && root.value?.contains(target)) {
    return
  }
  close()
}

onMounted(() => {
  document.addEventListener('pointerdown', onDocumentPointerDown)
})

onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', onDocumentPointerDown)
})
</script>

<template>
  <div ref="root" class="field">
    <span :id="labelId" class="field-label">{{ label }}</span>
    <div class="select-control">
      <button
        :id="fieldId"
        ref="trigger"
        type="button"
        class="select-trigger"
        role="combobox"
        aria-haspopup="listbox"
        :aria-expanded="open ? 'true' : 'false'"
        :aria-controls="listId"
        :aria-labelledby="labelId"
        :aria-invalid="invalid || error ? 'true' : undefined"
        :aria-describedby="error || hint ? messageId : undefined"
        :disabled="disabled"
        :data-open="open ? '' : undefined"
        @click="toggle"
        @keydown="onKeydown"
      >
        <span class="select-value" :class="{ 'is-placeholder': !selectedLabel }">
          {{ selectedLabel || (hasOptions ? placeholder : '暂无可选项') }}
        </span>
        <span class="select-caret" aria-hidden="true" />
      </button>
      <ul v-if="open" :id="listId" class="select-list" role="listbox">
        <li
          v-for="(option, index) in options"
          :key="option.value"
          class="select-option"
          :class="{ 'is-active': index === activeIndex }"
          role="option"
          :aria-selected="option.value === modelValue ? 'true' : 'false'"
          @click="select(option)"
          @pointermove="activeIndex = index"
        >
          {{ option.label }}
        </li>
      </ul>
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

.select-control {
  position: relative;
}

.select-trigger {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  width: 100%;
  height: 32px;
  padding: 0 var(--space-10);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-input);
  background: var(--bg-subtle);
  color: var(--text-1);
  font-size: var(--fs-body);
  text-align: left;
  cursor: pointer;
  transition:
    border-color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out);
}

.select-trigger:hover:not(:disabled) {
  border-color: var(--brand-line);
}

.select-trigger:focus-visible,
.select-trigger[data-open] {
  outline: none;
  border-color: var(--brand);
  box-shadow: var(--focus-ring);
}

.select-trigger[aria-invalid='true'] {
  border-color: var(--danger);
}

.select-trigger:disabled {
  opacity: var(--opacity-disabled);
  cursor: default;
}

.select-value {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.select-value.is-placeholder {
  color: var(--text-3);
}

.select-caret {
  width: 6px;
  height: 6px;
  border-right: 1.5px solid var(--text-3);
  border-bottom: 1.5px solid var(--text-3);
  transform: translateY(-2px) rotate(45deg);
  transition: transform var(--dur-fast) var(--ease-out);
}

.select-trigger[data-open] .select-caret {
  transform: translateY(1px) rotate(225deg);
}

.select-list {
  position: absolute;
  z-index: var(--z-sticky);
  top: calc(100% + var(--space-4));
  left: 0;
  right: 0;
  max-height: 240px;
  margin: 0;
  padding: var(--space-4);
  overflow-y: auto;
  list-style: none;
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  box-shadow: var(--shadow-md);
}

.select-option {
  padding: var(--space-8) var(--space-10);
  border-radius: var(--radius-tag);
  color: var(--text-1);
  font-size: var(--fs-body);
  cursor: pointer;
}

.select-option.is-active {
  background: var(--bg-subtle);
}

.select-option[aria-selected='true'] {
  background: var(--brand-surface);
  color: var(--brand-text);
  font-weight: var(--fw-medium);
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
</style>
