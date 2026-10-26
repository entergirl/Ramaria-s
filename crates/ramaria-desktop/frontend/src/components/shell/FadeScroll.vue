<script setup lang="ts">
/**
 * src/components/shell/FadeScroll.vue — 渐隐滚动区
 *
 * 职责:
 * - 承载定高容器内的纵向滚动，并在上下边缘做渐隐
 * - 暴露滚动位置读写，供长列表保存与恢复浏览快照
 *
 * 设计特点:
 * - 渐隐用遮罩实现，不额外插入渐变层，滚动条与内容同时淡出
 * - 上下留白收在渐隐区间内，首尾内容不被遮罩吃掉
 * - 只在需要「内容溢出即淡出」的场景复用；普通列表仍用原生滚动
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
  <div ref="el" class="fade-scroll">
    <slot />
  </div>
</template>

<style scoped>
.fade-scroll {
  min-height: 0;
  padding: var(--space-6) 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  /* 顶部渐隐收在 6px 内边距内，底部留出更长的过渡 */
  --fade-top: var(--space-6);
  --fade-bottom: var(--space-24);
  -webkit-mask-image: linear-gradient(
    180deg,
    transparent 0,
    var(--mask-opaque) var(--fade-top),
    var(--mask-opaque) calc(100% - var(--fade-bottom)),
    transparent 100%
  );
  mask-image: linear-gradient(
    180deg,
    transparent 0,
    var(--mask-opaque) var(--fade-top),
    var(--mask-opaque) calc(100% - var(--fade-bottom)),
    transparent 100%
  );
}

.fade-scroll::-webkit-scrollbar {
  width: 6px;
}

/* 滚动条滑块做成胶囊：半径取宽度的一半 */
.fade-scroll::-webkit-scrollbar-thumb {
  border-radius: calc(var(--space-6) / 2);
  background: var(--border-strong);
}

.fade-scroll::-webkit-scrollbar-track {
  background: transparent;
}
</style>
