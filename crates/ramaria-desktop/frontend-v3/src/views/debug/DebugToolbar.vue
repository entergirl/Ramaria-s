<script setup lang="ts">
/**
 * src/views/debug/DebugToolbar.vue — 调试页工具栏
 *
 * 职责:
 * - 承载三个页签的切换，并在右侧放置当前页签的刷新与目录选择动作
 *
 * 设计特点:
 * - 页签为分段控件，切换只改路由，由路由回写当前页签
 * - 动作由调用方经插槽注入，工具栏不感知各页签的取数细节
 * - 无页眉：工具栏本身就是页面顶部
 */

import BaseSegmented from '../../components/base/BaseSegmented.vue'
import type { DebugTab } from './useDebugView'

defineProps<{
  tab: DebugTab
  tabs: Array<{ value: DebugTab; label: string }>
}>()

defineEmits<{
  select: [value: string]
}>()
</script>

<template>
  <section class="toolbar">
    <BaseSegmented
      :model-value="tab"
      :options="tabs"
      label="调试页签"
      @update:model-value="$emit('select', $event)"
    />

    <span class="spacer" />

    <div class="actions">
      <slot name="actions" />
    </div>
  </section>
</template>

<style scoped>
.toolbar {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--space-14);
  padding: var(--space-14) var(--space-20);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-sm);
}

.spacer {
  flex: 1;
}

.actions {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--space-8);
}

@media (max-width: 720px) {
  .toolbar {
    flex-wrap: wrap;
  }
}
</style>
