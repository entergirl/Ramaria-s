<script setup lang="ts">
/**
 * src/views/setup/SetupStatusLine.vue — 向导行内状态
 *
 * 职责:
 * - 在固定高度的一行内表达「未开始 / 进行中 / 成功 / 失败」四种状态
 *
 * 设计特点:
 * - 状态不单靠颜色：成功与失败同时给符号与文案，进行中给省略号
 * - 高度固定，状态切换不推动下方内容
 */

defineProps<{
  tone: 'idle' | 'busy' | 'ok' | 'fail'
  text: string
}>()
</script>

<template>
  <p class="status" :data-tone="tone" role="status" aria-live="polite">
    <span v-if="tone === 'ok'" class="glyph" aria-hidden="true">✓</span>
    <span v-else-if="tone === 'fail'" class="glyph" aria-hidden="true">✗</span>
    <span class="text">{{ text }}</span>
  </p>
</template>

<style scoped>
.status {
  display: flex;
  align-items: center;
  gap: var(--space-6);
  min-height: 18px;
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.status[data-tone='busy'] {
  color: var(--text-2);
}

.status[data-tone='ok'] {
  color: var(--ok);
}

.status[data-tone='fail'] {
  color: var(--danger);
}

.glyph {
  font-weight: var(--fw-semibold);
}
</style>
