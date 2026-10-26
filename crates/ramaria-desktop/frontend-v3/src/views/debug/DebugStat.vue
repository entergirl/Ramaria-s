<script setup lang="ts">
/**
 * src/views/debug/DebugStat.vue — 调试视图的数值块
 *
 * 职责:
 * - 在关键词池与说话风格页展示单个计数或状态值
 *
 * 设计特点:
 * - 数值在上、标签在下，等宽字体承载数字
 * - 色调只表意（规范 / 已确认别名 / 待确认 / 正常 / 提示），不承担层级
 * - 可选补充说明跟在数值后，用于状态解释（如「样本低于阈值」）
 */

withDefaults(
  defineProps<{
    label: string
    value: string | number
    tone?: 'neutral' | 'brand' | 'accent' | 'warn' | 'ok'
    note?: string
  }>(),
  {
    tone: 'neutral',
    note: '',
  },
)
</script>

<template>
  <div class="stat" :data-tone="tone">
    <span class="stat-value">{{ value }}</span>
    <span class="stat-label">{{ label }}</span>
    <span v-if="note" class="stat-note">{{ note }}</span>
  </div>
</template>

<style scoped>
.stat {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
  padding: var(--space-10) var(--space-14);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-card);
}

.stat[data-tone='brand'] {
  border-color: var(--brand-line);
  background: var(--brand-surface);
}

.stat[data-tone='accent'] {
  border-color: var(--accent-line);
  background: var(--accent-surface);
}

.stat[data-tone='warn'] {
  border-color: var(--warn-line);
  background: var(--warn-surface);
}

.stat[data-tone='ok'] {
  border-color: var(--ok-line);
  background: var(--ok-surface);
}

.stat-value {
  color: var(--text-1);
  font-family: var(--font-mono);
  font-size: var(--fs-h2);
  font-weight: var(--fw-semibold);
  line-height: 1.2;
  overflow-wrap: anywhere;
}

.stat-label {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.stat-note {
  color: var(--text-2);
  font-size: var(--fs-caption);
  line-height: 1.4;
}
</style>
