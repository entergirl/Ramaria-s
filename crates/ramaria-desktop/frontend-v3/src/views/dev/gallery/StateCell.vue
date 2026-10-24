<script setup lang="ts">
/**
 * src/views/dev/gallery/StateCell.vue — 格位外框
 *
 * 职责:
 * - 承载一个状态格的编号、状态名、触发方式与组件实例
 *
 * 设计特点:
 * - 外框只负责标注与对齐，不干预内部组件的状态表达
 * - 触发方式与说明分列两行，避免把「怎么看」混进状态名
 */
import type { GalleryCell } from './catalog'

defineProps<{
  cell: GalleryCell
  code: string
}>()
</script>

<template>
  <article class="cell" :data-cell="cell.id" :data-qa="code">
    <header class="cell-head">
      <span class="cell-code">{{ code }}</span>
      <span class="cell-label">{{ cell.label }}</span>
    </header>
    <div class="cell-body">
      <slot />
    </div>
    <p v-if="cell.trigger" class="cell-hint">触发：{{ cell.trigger }}</p>
    <p v-if="cell.note" class="cell-hint">说明：{{ cell.note }}</p>
  </article>
</template>

<style scoped>
.cell {
  display: flex;
  flex-direction: column;
  gap: var(--space-10);
  min-width: 0;
  padding: var(--space-12);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-card);
  box-shadow: var(--shadow-xs);
}

.cell-head {
  display: flex;
  align-items: baseline;
  gap: var(--space-8);
  min-width: 0;
}

.cell-code {
  flex: none;
  color: var(--text-3);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
}

.cell-label {
  min-width: 0;
  overflow: hidden;
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* 下拉等浮层需要溢出格位，此处不裁剪；内容自身按宽度收缩 */
.cell-body {
  display: flex;
  flex: 1;
  flex-direction: column;
  justify-content: center;
  gap: var(--space-8);
  min-height: 96px;
  padding: var(--space-10);
  border-radius: var(--radius-tag);
  background: var(--bg-subtle);
}

.cell-hint {
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: var(--lh-caption);
}
</style>
