<script setup lang="ts">
/**
 * src/components/shell/DownloadBanner.vue — 模型下载全局横幅
 *
 * 职责:
 * - 展示进行中 / 失败两态下载进度，不切换视图、不阻塞其它操作
 *
 * 设计特点:
 * - 固定顶部居中，`pointer-events: none` 不夺焦；终态收起由 app store 计时
 * - 进度组件带阶段文案与百分比，不出现无说明的进度条
 */
import { computed } from 'vue'

import BaseProgress from '../base/BaseProgress.vue'
import { downloadBannerView } from '../../core/download'
import { useAppStore } from '../../stores/app'

const app = useAppStore()
const view = computed(() => downloadBannerView(app.download))
</script>

<template>
  <Transition name="download">
    <section
      v-if="view"
      class="download-banner"
      role="status"
      aria-live="polite"
      :data-state="view.state"
    >
      <BaseProgress :value="view.value" :state="view.state" :label="view.label" />
    </section>
  </Transition>
</template>

<style scoped>
.download-banner {
  position: fixed;
  top: var(--space-12);
  left: 50%;
  z-index: var(--z-veil);
  width: min(560px, calc(100% - var(--space-48)));
  padding: var(--space-12) var(--space-16);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  background: var(--bg-card);
  box-shadow: var(--shadow-md);
  transform: translateX(-50%);
  pointer-events: none;
}

.download-banner[data-state='failed'] {
  border-color: var(--danger-line);
  background: var(--danger-surface);
}

.download-enter-active,
.download-leave-active {
  transition:
    opacity var(--dur-fast) var(--ease-out),
    transform var(--dur-fast) var(--ease-out);
}

.download-enter-from,
.download-leave-to {
  opacity: 0;
  transform: translate(-50%, calc(-1 * var(--space-8)));
}
</style>
