<script setup lang="ts">
/**
 * src/components/shell/FullscreenShell.vue — 全屏页舞台
 *
 * 职责:
 * - 为首次配置与错误页提供居中舞台与宽度约束
 *
 * 设计特点:
 * - 全屏页复用圆键导航承担品牌与主题切换，本组件不再单独渲染页角操作
 * - 底部留出圆键导航的浮层空间，内容不被常驻导航遮挡
 * - 只做布局，不承载页面文案与状态；卡片宽度由各页面自行约束
 */
</script>

<template>
  <main class="stage">
    <div class="content">
      <slot />
    </div>
  </main>
</template>

<style scoped>
.stage {
  display: grid;
  place-items: center;
  min-height: 100%;
  /* 圆键在全屏页位于右侧，卡片不再被它遮挡，底部只留呼吸间距 */
  padding: var(--space-32) var(--space-24) var(--space-24);
}

/* 舞台满宽：卡片宽度由页面按自身内容约束（setup 800、error 520） */
.content {
  width: 100%;
}

/* 极矮窗口：把舞台留白让给卡片，避免正文滚动 */
@media (max-height: 520px) {
  .stage {
    padding: var(--space-16);
  }
}

/* 窄窗：卡片占满宽度，改顶部对齐，避免底部被圆键覆盖 */
@media (max-width: 900px) {
  .stage {
    align-items: start;
  }
}
</style>
