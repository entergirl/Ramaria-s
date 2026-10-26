<script setup lang="ts">
/**
 * src/views/settings/SchemaGroupCard.vue — 键表驱动的设置卡
 *
 * 职责:
 * - 按组声明渲染字段（组内可再分小节），并承载该组的保存 / 恢复默认动作
 *
 * 设计特点:
 * - 值从草稿文档按路径读取，不在卡片内保存副本
 * - 组内小节只影响呈现顺序；未列入小节的字段追加在末尾，避免漏项
 * - 错误条在卡内，重试即再次保存
 */

import { computed } from 'vue'

import { readPath } from '../../config/configDraft'
import type { ConfigDocument, JsonValue } from '../../config/configTypes'
import type { SettingField, SettingGroup, SettingOption } from '../../config/schema/define'
import SchemaField from './fields/SchemaField.vue'
import SettingsCard from './SettingsCard.vue'

const props = withDefaults(
  defineProps<{
    group: SettingGroup
    document: ConfigDocument
    dirty?: boolean
    saving?: boolean
    error?: string
    resetDisabled?: boolean
    resetHint?: string
    /** 动态选项覆盖（按字段键）：人格名单等运行时选项 */
    optionOverrides?: Record<string, SettingOption[]>
  }>(),
  {
    dirty: false,
    saving: false,
    error: '',
    resetDisabled: false,
    resetHint: '',
    optionOverrides: () => ({}),
  },
)

const emit = defineEmits<{
  update: [field: SettingField, value: JsonValue]
  save: []
  reset: []
}>()

interface RenderBlock {
  title: string
  fields: SettingField[]
}

/** 渲染顺序：先按小节，未列入小节的字段收进末尾的「其它」组 */
const blocks = computed<RenderBlock[]>(() => {
  const declared = props.group.blocks
  if (!declared || declared.length === 0) {
    return [{ title: '', fields: props.group.fields }]
  }
  const byKey = new Map(props.group.fields.map((field) => [field.key, field]))
  const used = new Set<string>()
  const result: RenderBlock[] = []
  for (const block of declared) {
    const fields: SettingField[] = []
    for (const key of block.keys) {
      const field = byKey.get(key)
      if (field) {
        fields.push(field)
        used.add(key)
      }
    }
    if (fields.length > 0) {
      result.push({ title: block.title, fields })
    }
  }
  const rest = props.group.fields.filter((field) => !used.has(field.key))
  if (rest.length > 0) {
    result.push({ title: '', fields: rest })
  }
  return result
})

function valueOf(field: SettingField): JsonValue | undefined {
  return readPath(props.document, field.path)
}

/** 字段渲染声明：动态选项覆盖只替换 options，其余沿用组声明 */
function fieldFor(field: SettingField): SettingField {
  const override = props.optionOverrides[field.key]
  return override ? { ...field, options: override } : field
}

/** 宽控件独占整行：输入与选择需要完整可读宽度，带说明的开关同理 */
function isWide(field: SettingField): boolean {
  if (field.layout === 'auto') {
    return false
  }
  if (field.control === 'text' || field.control === 'select') {
    return true
  }
  if (field.control === 'whitelist' || field.control === 'order') {
    return true
  }
  return field.control === 'switch' && !!field.hint
}

/** 组声明的固定列数；缺省交给 auto-fit */
const gridClass = computed(() => (props.group.columns ? `is-cols-${props.group.columns}` : ''))
</script>

<template>
  <SettingsCard
    :title="group.title"
    :description="group.description"
    :dirty="dirty"
    :saving="saving"
    :error="error"
    :reset-disabled="resetDisabled"
    :reset-hint="resetHint"
    @save="emit('save')"
    @reset="emit('reset')"
  >
    <template #head>
      <slot name="head" />
    </template>

    <div v-for="(block, index) in blocks" :key="`${group.id}-${index}`" class="s-block">
      <h4 v-if="block.title" class="s-block-title">{{ block.title }}</h4>
      <div class="s-block-grid" :class="gridClass">
        <SchemaField
          v-for="field in block.fields"
          :key="field.key"
          :class="{ 'is-wide': isWide(field) }"
          :field="fieldFor(field)"
          :value="valueOf(field)"
          :disabled="saving"
          @update="emit('update', field, $event)"
        />
      </div>
    </div>

    <slot />

    <template #footExtra>
      <slot name="footExtra" />
    </template>
  </SettingsCard>
</template>

<style scoped>
.s-block {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
}

.s-block + .s-block {
  padding-top: var(--space-10);
  border-top: 1px dashed var(--border);
}

.s-block-title {
  margin: 0;
  color: var(--text-2);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
  letter-spacing: 0.02em;
}

/* 组内字段自适应列数：窄卡回落单列，宽卡最多排四到五列 */
.s-block-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(148px, 1fr));
  gap: var(--space-10) var(--space-14);
  align-items: start;
}

/* 组声明的固定列数：小卡片按需拉齐，不被 auto-fit 留出空列 */
.s-block-grid.is-cols-2 {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.s-block-grid.is-cols-3 {
  grid-template-columns: repeat(3, minmax(0, 1fr));
}

.s-block-grid.is-cols-4 {
  grid-template-columns: repeat(4, minmax(0, 1fr));
}

/* 输入类控件与带说明的开关独占整行，避免标签与提示被压成窄条 */
.s-block-grid :deep(.is-wide) {
  grid-column: 1 / -1;
}
</style>
