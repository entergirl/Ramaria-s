<script setup lang="ts">
/**
 * src/components/base/PlaceholderView.vue — 视图占位
 *
 * 职责:
 * - 为尚未落成的视图提供可识别、无交互的渲染出口
 *
 * 设计特点:
 * - 只渲染标题与一句说明，不提供操作入口，避免被误认作已完成的空态
 * - 视觉取值全部来自令牌，不引入局部字面量
 */

defineProps<{
  title: string
  note: string
}>()
</script>

<template>
  <section class="placeholder">
    <h1 class="placeholder-title">{{ title }}</h1>
    <p class="placeholder-note">{{ note }}</p>
  </section>
</template>

<style scoped>
.placeholder {
  display: flex;
  flex-direction: column;
  gap: var(--space-12);
  align-items: center;
  justify-content: center;
  height: 100%;
  padding: var(--space-40) var(--space-24);
  text-align: center;
}

.placeholder-title {
  font-family: var(--font-title);
  font-size: var(--fs-h1);
  line-height: var(--lh-h1);
  font-weight: var(--fw-semibold);
}

.placeholder-note {
  color: var(--text-3);
  font-size: var(--fs-small);
}
</style>
