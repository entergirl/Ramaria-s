<script setup lang="ts">
/**
 * src/components/base/BaseSkeleton.vue — 骨架占位
 *
 * 职责:
 * - 在内容到达前占位，形状与真实内容一致，避免加载完成后跳动
 *
 * 设计特点:
 * - 形状由 `shape` 决定，文本与表格按 `lines` 数量展开
 * - 动效为横向微光；`motion="static"` 与减弱动效偏好下转为静态块
 * - 列表场景由调用方控制最多三张卡，组件本身不限制数量
 */
withDefaults(
  defineProps<{
    shape?: 'text' | 'avatar' | 'card' | 'bubble' | 'table'
    lines?: number
    motion?: 'shimmer' | 'static'
  }>(),
  {
    shape: 'text',
    lines: 3,
    motion: 'shimmer',
  },
)
</script>

<template>
  <div class="skeleton" :data-shape="shape" :data-motion="motion" aria-hidden="true">
    <template v-if="shape === 'card'">
      <div class="sk-head">
        <span class="sk-block sk-avatar" />
        <span class="sk-block sk-line sk-line--short" />
      </div>
      <span v-for="index in lines" :key="index" class="sk-block sk-line" />
    </template>
    <template v-else-if="shape === 'bubble'">
      <span class="sk-block sk-line sk-line--bubble" />
      <span class="sk-block sk-line sk-line--bubble-short" />
    </template>
    <template v-else-if="shape === 'table'">
      <span v-for="index in lines" :key="index" class="sk-block sk-row" />
    </template>
    <template v-else-if="shape === 'avatar'">
      <span class="sk-block sk-avatar" />
    </template>
    <template v-else>
      <span v-for="index in lines" :key="index" class="sk-block sk-line" />
    </template>
  </div>
</template>

<style scoped>
.skeleton {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
  width: 100%;
}

.sk-head {
  display: flex;
  align-items: center;
  gap: var(--space-8);
}

.sk-block {
  display: block;
  border-radius: var(--radius-tag);
  background: var(--bg-sunken);
  background-image: linear-gradient(
    90deg,
    transparent,
    var(--bg-subtle) 40%,
    transparent 80%
  );
  background-size: 200% 100%;
  animation: sk-shimmer var(--dur-shimmer) var(--ease-in-out) infinite;
}

.skeleton[data-motion='static'] .sk-block {
  animation: none;
  background-image: none;
}

.sk-line {
  height: 10px;
}

.sk-line:nth-child(2n) {
  width: 72%;
}

.sk-line--short {
  width: 40%;
}

.sk-line--bubble {
  height: 14px;
  width: 220px;
  border-radius: var(--radius-bubble);
}

.sk-line--bubble-short {
  height: 14px;
  width: 140px;
  border-radius: var(--radius-bubble);
}

.sk-avatar {
  width: 32px;
  height: 32px;
  border-radius: var(--radius-full);
}

.sk-row {
  height: 24px;
  border-radius: var(--radius-tag);
}

@keyframes sk-shimmer {
  from {
    background-position: 160% 0;
  }

  to {
    background-position: -60% 0;
  }
}

@media (prefers-reduced-motion: reduce) {
  .sk-block {
    animation: none;
    background-image: none;
  }
}
</style>
