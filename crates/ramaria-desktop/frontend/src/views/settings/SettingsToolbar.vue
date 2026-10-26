<script setup lang="ts">
/**
 * src/views/settings/SettingsToolbar.vue — 设置页的两级目录
 *
 * 职责:
 * - 一级切换基础 / 进阶范围（进阶在调试未解锁时不渲染）
 * - 二级切换当前范围的子页，并在分类键上标出该类未保存项数量
 *
 * 设计特点:
 * - 两级目录各自是居中胶囊：一级用分段控件，二级用统一背板的分段键条
 * - 无页眉：目录本身就是页面顶部，内容在其下独立滚动
 * - 未保存数量以键上的小徽标呈现，不额外占一行；同时提供读屏状态文本
 * - 窄窗下二级键条在背板内换行，背板圆角回落为矩形圆角
 */

import BaseSegmented from '../../components/base/BaseSegmented.vue'
import { SCOPE_LABELS, type SettingsScope, type SettingsSection } from '../../config/settingsSections'

const props = defineProps<{
  scope: SettingsScope
  sectionId: string
  sections: SettingsSection[]
  /** 各子页未保存的字段数（键为子页 id） */
  dirtyCounts: Record<string, number>
  advancedUnlocked: boolean
}>()

const emit = defineEmits<{
  'select-scope': [scope: SettingsScope]
  'select-section': [sectionId: string]
}>()

function scopeOptions() {
  const options: Array<{ value: string; label: string }> = [
    { value: 'basic', label: SCOPE_LABELS.basic },
  ]
  if (props.advancedUnlocked) {
    options.push({ value: 'advanced', label: SCOPE_LABELS.advanced })
  }
  return options
}

function onScope(value: string): void {
  emit('select-scope', value as SettingsScope)
}

function countOf(sectionId: string): number {
  return props.dirtyCounts[sectionId] ?? 0
}

/** 读屏状态：当前页与全部子页的未保存项，视觉上由键上徽标承担 */
function dirtyStatusText(): string {
  const active = countOf(props.sectionId)
  if (active > 0) {
    return `${SCOPE_LABELS[props.scope]}当前分类有 ${active} 项未保存`
  }
  return ''
}
</script>

<template>
  <div class="s-toolbar">
    <BaseSegmented
      v-if="advancedUnlocked"
      class="s-scope"
      :model-value="scope"
      :options="scopeOptions()"
      size="lg"
      pill
      label="设置范围"
      @update:model-value="onScope"
    />

    <nav class="s-sections" role="tablist" :aria-label="`${SCOPE_LABELS[scope]}分类`">
      <button
        v-for="item in sections"
        :key="item.id"
        type="button"
        role="tab"
        class="s-chip"
        :class="{ 'is-active': item.id === sectionId }"
        :aria-selected="item.id === sectionId"
        :aria-label="countOf(item.id) > 0 ? `${item.label}，${countOf(item.id)} 项未保存` : undefined"
        @click="emit('select-section', item.id)"
      >
        <span>{{ item.label }}</span>
        <span v-if="countOf(item.id) > 0" class="s-chip-count" aria-hidden="true">
          {{ countOf(item.id) }}
        </span>
      </button>
    </nav>

    <span class="s-sr" role="status" aria-live="polite">{{ dirtyStatusText() }}</span>
  </div>
</template>

<style scoped>
.s-toolbar {
  display: flex;
  flex: none;
  flex-direction: column;
  align-items: center;
  gap: var(--space-8);
}

/* 一级目录：在分段控件背板外补描边与投影，与二级背板同一种容器语言 */
.s-scope {
  border: 1px solid var(--border);
  box-shadow: var(--shadow-xs);
}

/* 二级目录：统一背板承托全部分类键，居中且随内容收窄 */
.s-sections {
  display: flex;
  flex-wrap: wrap;
  justify-content: center;
  gap: var(--space-4);
  max-width: 100%;
  padding: var(--space-4);
  border: 1px solid var(--border);
  border-radius: var(--radius-pill);
  background: var(--bg-subtle);
  box-shadow: var(--shadow-xs);
}

.s-chip {
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
  white-space: nowrap;
  cursor: pointer;
  transition:
    background-color var(--dur-fast) var(--ease-out),
    border-color var(--dur-fast) var(--ease-out),
    color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out);
}

.s-chip:hover:not(.is-active) {
  background: var(--bg-card);
  color: var(--brand-text);
}

.s-chip.is-active {
  background: var(--btn-bg);
  color: var(--btn-fg);
  box-shadow: var(--btn-shadow);
}

.s-chip:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.s-chip-count {
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

.s-sr {
  position: absolute;
  width: 1px;
  height: 1px;
  overflow: hidden;
  clip-path: inset(50%);
  white-space: nowrap;
}

@media (max-width: 720px) {
  .s-sections {
    border-radius: var(--radius-inner);
  }
}
</style>
