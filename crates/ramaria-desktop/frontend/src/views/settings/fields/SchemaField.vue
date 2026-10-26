<script setup lang="ts">
/**
 * src/views/settings/fields/SchemaField.vue — 按声明渲染单个设置项
 *
 * 职责:
 * - 按字段类型渲染开关 / 数值 / 文本 / 下拉 / 白名单 / 优先级列表
 * - 数值输入的就地校验与单位展示
 *
 * 设计特点:
 * - 控件复用基础组件（BaseSwitch / BaseInput / BaseSelect / BaseCheckbox / BaseButton）
 * - 只发出合法值：数值越界或非数字时保留输入并提示，不写脏值
 * - 白名单与优先级列表的语义与配置口径一致（`*` 与具体项互斥、顺序即优先级）
 */

import { computed, ref, useId, watch } from 'vue'

import BaseButton from '../../../components/base/BaseButton.vue'
import BaseCheckbox from '../../../components/base/BaseCheckbox.vue'
import BaseInput from '../../../components/base/BaseInput.vue'
import BaseSelect from '../../../components/base/BaseSelect.vue'
import BaseSwitch from '../../../components/base/BaseSwitch.vue'
import type { JsonValue } from '../../../config/configTypes'
import type { SettingField } from '../../../config/schema/define'

const props = withDefaults(
  defineProps<{
    field: SettingField
    value: JsonValue | undefined
    disabled?: boolean
  }>(),
  { disabled: false },
)

const emit = defineEmits<{
  update: [value: JsonValue]
}>()

const inputId = useId()
const numberText = ref('')
const numberError = ref('')

const labelText = computed(() =>
  props.field.unit ? `${props.field.label}（${props.field.unit}）` : props.field.label,
)

/** 生效方式提示：只在字段显式声明时追加，避免每行都写「下次使用生效」 */
const applyText = computed(() => {
  if (props.field.apply === 'hot') {
    return '保存后立即生效。'
  }
  if (props.field.apply === 'restart') {
    return '重启应用后生效。'
  }
  return ''
})

const hintText = computed(() =>
  [props.field.hint, applyText.value].filter((item) => !!item).join(' '),
)

const checked = computed(() => props.value === true)

const textValue = computed(() => (typeof props.value === 'string' ? props.value : ''))

const selectValue = computed(() => (typeof props.value === 'string' ? props.value : ''))

const selectedList = computed<string[]>(() =>
  Array.isArray(props.value) ? props.value.filter((item): item is string => typeof item === 'string') : [],
)

const orderList = computed<string[]>(() => {
  const known = new Set((props.field.options ?? []).map((option) => option.value))
  const picked = selectedList.value.filter((value) => known.has(value))
  const rest = (props.field.options ?? [])
    .map((option) => option.value)
    .filter((value) => !picked.includes(value))
  return [...picked, ...rest]
})

const numberBoundHint = computed(() => {
  const { min, max } = props.field
  if (min !== undefined && max !== undefined) {
    return `范围 ${min} ~ ${max}`
  }
  if (min !== undefined) {
    return `不小于 ${min}`
  }
  if (max !== undefined) {
    return `不大于 ${max}`
  }
  return ''
})

watch(
  () => props.value,
  (value) => {
    const next = typeof value === 'number' ? value : null
    if (next !== null && Number(numberText.value) === next) {
      return
    }
    numberText.value = next === null ? '' : String(next)
    numberError.value = ''
  },
  { immediate: true },
)

function onSwitch(value: boolean): void {
  emit('update', value)
}

function onText(value: string): void {
  emit('update', value)
}

function onSelect(value: string): void {
  emit('update', value)
}

function onNumberInput(raw: string): void {
  numberText.value = raw
  const trimmed = raw.trim()
  if (!trimmed) {
    numberError.value = '请输入数字'
    return
  }
  const parsed = Number(trimmed)
  if (!Number.isFinite(parsed)) {
    numberError.value = '请输入数字'
    return
  }
  const { min, max } = props.field
  if (min !== undefined && parsed < min) {
    numberError.value = `不能小于 ${min}`
    return
  }
  if (max !== undefined && parsed > max) {
    numberError.value = `不能大于 ${max}`
    return
  }
  numberError.value = ''
  emit('update', parsed)
}

function toggleWhitelist(value: string): void {
  if (props.disabled) {
    return
  }
  const current = selectedList.value
  let next: string[]
  if (value === '*') {
    next = current.includes('*') ? [] : ['*']
  } else {
    const withoutWildcard = current.filter((item) => item !== '*')
    next = withoutWildcard.includes(value)
      ? withoutWildcard.filter((item) => item !== value)
      : [...withoutWildcard, value]
  }
  emit('update', next)
}

function moveOrder(index: number, step: number): void {
  const list = [...orderList.value]
  const target = index + step
  if (target < 0 || target >= list.length) {
    return
  }
  const [item] = list.splice(index, 1)
  if (item === undefined) {
    return
  }
  list.splice(target, 0, item)
  emit('update', list)
}

function optionLabel(value: string): string {
  return props.field.options?.find((option) => option.value === value)?.label ?? value
}
</script>

<template>
  <div class="s-field" :data-kind="field.control" :data-field="field.key">
    <!-- 开关：标签与控件同行 -->
    <template v-if="field.control === 'switch'">
      <div class="s-field-row">
        <div class="s-field-text">
          <span class="s-field-label">{{ labelText }}</span>
          <p v-if="hintText" class="s-field-hint">{{ hintText }}</p>
        </div>
        <BaseSwitch
          :model-value="checked"
          :disabled="disabled"
          :aria-label="field.label"
          @update:model-value="onSwitch"
        />
      </div>
    </template>

    <!-- 数值 -->
    <template v-else-if="field.control === 'number'">
      <BaseInput
        :id="inputId"
        :model-value="numberText"
        :label="labelText"
        type="number"
        :min="field.min"
        :max="field.max"
        :step="field.step"
        :hint="[hintText, numberBoundHint].filter((item) => !!item).join(' ')"
        :error="numberError"
        :invalid="!!numberError"
        :disabled="disabled"
        @update:model-value="onNumberInput"
      />
    </template>

    <!-- 文本 -->
    <template v-else-if="field.control === 'text'">
      <BaseInput
        :id="inputId"
        :model-value="textValue"
        :label="labelText"
        :hint="hintText"
        :disabled="disabled"
        @update:model-value="onText"
      />
    </template>

    <!-- 下拉 -->
    <template v-else-if="field.control === 'select'">
      <BaseSelect
        :id="inputId"
        :model-value="selectValue"
        :label="labelText"
        :options="field.options ?? []"
        :hint="hintText"
        :disabled="disabled"
        @update:model-value="onSelect"
      />
    </template>

    <!-- 白名单 -->
    <template v-else-if="field.control === 'whitelist'">
      <div class="s-field-block">
        <span class="s-field-label">{{ labelText }}</span>
        <div class="s-field-options">
          <BaseCheckbox
            v-for="option in field.options ?? []"
            :key="option.value"
            :model-value="selectedList.includes(option.value)"
            :label="option.label"
            :disabled="disabled"
            @update:model-value="toggleWhitelist(option.value)"
          />
        </div>
        <p v-if="field.hint" class="s-field-hint">{{ field.hint }}</p>
      </div>
    </template>

    <!-- 优先级列表 -->
    <template v-else>
      <div class="s-field-block">
        <span class="s-field-label">{{ labelText }}</span>
        <ol class="s-order">
          <li v-for="(item, index) in orderList" :key="item" class="s-order-row">
            <span class="s-order-index">{{ index + 1 }}</span>
            <span class="s-order-label">{{ optionLabel(item) }}</span>
            <BaseButton
              variant="ghost"
              size="sm"
              :disabled="disabled || index === 0"
              :aria-label="`上移${optionLabel(item)}`"
              @click="moveOrder(index, -1)"
            >
              上移
            </BaseButton>
            <BaseButton
              variant="ghost"
              size="sm"
              :disabled="disabled || index === orderList.length - 1"
              :aria-label="`下移${optionLabel(item)}`"
              @click="moveOrder(index, 1)"
            >
              下移
            </BaseButton>
          </li>
        </ol>
        <p v-if="field.hint" class="s-field-hint">{{ field.hint }}</p>
      </div>
    </template>
  </div>
</template>

<style scoped>
.s-field {
  min-width: 0;
}

.s-field-row {
  display: flex;
  align-items: flex-start;
  gap: var(--space-12);
}

.s-field-text {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
}

.s-field-label {
  color: var(--text-2);
  font-size: var(--fs-small);
  font-weight: var(--fw-medium);
  line-height: 1.5;
}

.s-field-hint {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
}

.s-field-block {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
  min-width: 0;
}

.s-field-options {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-10);
}

.s-order {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  margin: 0;
  padding: 0;
  list-style: none;
}

.s-order-row {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  padding: var(--space-4) var(--space-8);
  border: 1px solid var(--border);
  border-radius: var(--radius-tag);
  background: var(--bg-sunken);
}

.s-order-index {
  width: 16px;
  color: var(--text-3);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
  text-align: center;
}

.s-order-label {
  flex: 1;
  color: var(--text-1);
  font-size: var(--fs-small);
}
</style>
