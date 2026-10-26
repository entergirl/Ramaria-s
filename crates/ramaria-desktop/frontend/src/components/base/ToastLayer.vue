<script setup lang="ts">
/**
 * src/components/base/ToastLayer.vue — 轻提示层
 *
 * 职责:
 * - 订阅提示队列并渲染为右上角堆叠列表
 *
 * 设计特点:
 * - 最多三条同时在场，新提示从下方进入
 * - 承载操作的提示在点击操作后一并关闭
 * - 层级取 `--z-toast`，位于模态之上、工具提示之下
 */

import BaseToast from './BaseToast.vue'
import { dismissToast, toasts } from '../../core/toast'

function runAction(id: number, run: () => void): void {
  run()
  dismissToast(id)
}
</script>

<template>
  <div class="toast-layer" role="region" aria-label="通知">
    <TransitionGroup name="toast">
      <BaseToast
        v-for="item in toasts"
        :key="item.id"
        :tone="item.tone"
        :message="item.message"
        :action-label="item.action ? item.action.label : ''"
        @activate="item.action ? runAction(item.id, item.action.run) : undefined"
        @close="dismissToast(item.id)"
      />
    </TransitionGroup>
  </div>
</template>

<style scoped>
.toast-layer {
  position: fixed;
  top: var(--space-16);
  right: var(--space-16);
  z-index: var(--z-toast);
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
  pointer-events: none;
}

.toast-layer > * {
  pointer-events: auto;
}

.toast-enter-active,
.toast-leave-active {
  transition:
    opacity var(--dur-base) var(--ease-out),
    transform var(--dur-base) var(--ease-pop);
}

.toast-enter-from {
  opacity: 0;
  transform: translateY(-6px) scale(0.98);
}

.toast-leave-to {
  opacity: 0;
  transform: translateY(-4px);
}

@media (prefers-reduced-motion: reduce) {
  .toast-enter-active,
  .toast-leave-active {
    transition: none;
  }
}
</style>
