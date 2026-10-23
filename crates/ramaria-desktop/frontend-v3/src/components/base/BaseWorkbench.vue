<script setup lang="ts">
/**
 * src/components/base/BaseWorkbench.vue — 工作台容器
 *
 * 职责:
 * - 约束内容区宽度与内边距，使主视图在宽窗下保持居中单列
 *
 * 设计特点:
 * - 宽度取 `--w-workbench`（880），窄窗自动收缩，不出现横向滚动
 * - 底部留出圆键导航的浮层空间，内容不被常驻导航遮挡
 * - 只负责布局，不承担背景光斑（背景在 body 上一次性绘制）
 */
</script>

<template>
  <div class="workbench">
    <slot />
  </div>
</template>

<style scoped>
.workbench {
  width: 100%;
  max-width: var(--w-workbench);
  margin: 0 auto;
  padding: var(--space-40) var(--space-24) calc(var(--space-48) + var(--space-40));
}
</style>
