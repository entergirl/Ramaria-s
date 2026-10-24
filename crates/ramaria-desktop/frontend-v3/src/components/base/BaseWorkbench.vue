<script setup lang="ts">
/**
 * src/components/base/BaseWorkbench.vue — 工作台容器
 *
 * 职责:
 * - 约束内容区宽度与内边距，使主视图在宽窗下保持居中单列
 * - 以整屏舞台承载视图，视图在可用高度内上下居中，高于一屏时舞台滚动兜底
 *
 * 设计特点:
 * - 宽度取 `--w-workbench`（880），窄窗自动收缩，不出现横向滚动
 * - 高度取整屏，上下留白对称，不为圆键预留位置；圆键由用户拖动避让
 * - `safe center` 在内容高于可视区时退回顶部对齐，不裁掉首屏内容
 * - 只负责布局，不承担背景光斑（背景在 body 上一次性绘制）
 * - 暴露滚动位置读写，供长页面视图保存与恢复浏览快照
 */

import { ref } from 'vue'

const el = ref<HTMLElement | null>(null)

/** 当前滚动位置 */
function scrollTop(): number {
  return el.value?.scrollTop ?? 0
}

/** 滚动到指定位置（超出内容高度时由浏览器收敛） */
function scrollTo(top: number): void {
  if (el.value) {
    el.value.scrollTop = top
  }
}

defineExpose({ scrollTop, scrollTo })
</script>

<template>
  <div ref="el" class="workbench">
    <slot />
  </div>
</template>

<style scoped>
.workbench {
  display: flex;
  flex-direction: column;
  justify-content: center;
  justify-content: safe center;
  width: 100%;
  max-width: var(--w-workbench);
  height: 100vh;
  min-height: 0;
  margin: 0 auto;
  padding: var(--space-40) var(--space-24);
  overflow: auto;
}
</style>
