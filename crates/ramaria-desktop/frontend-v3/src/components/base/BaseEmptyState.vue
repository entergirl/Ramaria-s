<script setup lang="ts">
/**
 * src/components/base/BaseEmptyState.vue — 空态
 *
 * 职责:
 * - 表达真无数据、筛选无结果、能力缺失三种归因
 *
 * 设计特点:
 * - 说明最多两行；主操作至多一个，由调用方经默认插槽提供
 * - 图形用内联 SVG，经 `mark` 插槽传入，组件不内置插画
 * - 受阻形态用警示色，其余用中性色，不以颜色单独表意
 */

withDefaults(
  defineProps<{
    variant?: 'guide' | 'action' | 'blocked'
    title: string
    description: string
  }>(),
  {
    variant: 'guide',
  },
)
</script>

<template>
  <section class="empty" :data-variant="variant">
    <span class="empty-mark" aria-hidden="true">
      <slot name="mark">
        <svg viewBox="0 0 24 24">
          <circle cx="12" cy="12" r="9" />
          <path d="M8 13.5h8" />
        </svg>
      </slot>
    </span>
    <h2 class="empty-title">{{ title }}</h2>
    <p class="empty-desc">{{ description }}</p>
    <div v-if="$slots.default" class="empty-action">
      <slot />
    </div>
  </section>
</template>

<style scoped>
.empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--space-8);
  padding: var(--space-32) var(--space-24);
  text-align: center;
}

.empty-mark {
  display: grid;
  place-items: center;
  width: 48px;
  height: 48px;
  margin-bottom: var(--space-4);
  border-radius: var(--radius-full);
  background: var(--bg-subtle);
  color: var(--text-3);
}

.empty[data-variant='blocked'] .empty-mark {
  background: var(--warn-surface);
  color: var(--warn);
}

.empty-mark :deep(svg) {
  width: 22px;
  height: 22px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.empty-title {
  font-size: var(--fs-h3);
  line-height: var(--lh-h3);
  font-weight: var(--fw-semibold);
  color: var(--text-1);
}

.empty-desc {
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  max-width: 42ch;
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.empty-action {
  margin-top: var(--space-8);
}
</style>
