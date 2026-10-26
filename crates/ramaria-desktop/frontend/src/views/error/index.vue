<script setup lang="ts">
/**
 * src/views/error/index.vue — 不可恢复错误视图
 *
 * 职责:
 * - 在全屏态展示错误标题与一句原因，并提供唯一的「重试」动作
 *
 * 设计特点:
 * - 最简兜底：不动应用状态机、不显示堆栈与内部路径，技术细节留给诊断包
 * - 重试重新读取应用状态；根节点在重试中切到加载态，结束后回到错误态
 */

import { computed, ref } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import FullscreenShell from '../../components/shell/FullscreenShell.vue'
import { stateCopy } from '../../core/stateCopy'
import { useAppStore } from '../../stores/app'

const app = useAppStore()
const retrying = ref(false)
const copy = stateCopy('error.detail')

/** 后端返回的错误短句（为空时不渲染） */
const detail = computed(() => app.bootError.trim())

async function retry(): Promise<void> {
  if (retrying.value) {
    return
  }
  retrying.value = true
  try {
    await app.bootstrap(true)
  } finally {
    retrying.value = false
  }
}
</script>

<template>
  <FullscreenShell
    :data-view-state="retrying ? 'loading' : 'error'"
    :aria-busy="retrying ? 'true' : undefined"
  >
    <section class="error">
      <span class="error-mark" aria-hidden="true">
        <svg viewBox="0 0 24 24">
          <path d="M12 9v4" />
          <path d="M12 17h.01" />
          <path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
        </svg>
      </span>
      <BaseTag tone="danger">启动失败</BaseTag>
      <h1 class="error-title">{{ copy.title }}</h1>
      <p class="error-desc">{{ copy.description }}</p>
      <p v-if="detail" class="error-detail">{{ detail }}</p>
      <BaseButton variant="primary" :loading="retrying" @click="retry">重试</BaseButton>
    </section>
  </FullscreenShell>
</template>

<style scoped>
.error {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--space-12);
  max-width: 520px;
  margin: 0 auto;
  padding: var(--space-24) var(--space-16);
  text-align: center;
}

.error-mark {
  display: grid;
  place-items: center;
  width: 52px;
  height: 52px;
  border: 1px solid var(--danger-line);
  border-radius: var(--radius-full);
  background: var(--danger-surface);
  color: var(--danger);
}

.error-mark svg {
  width: 26px;
  height: 26px;
  fill: none;
  stroke: currentColor;
  stroke-width: 2;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.error-title {
  font-family: var(--font-title);
  font-size: var(--fs-display);
  font-weight: var(--fw-semibold);
  line-height: var(--lh-display);
}

.error-desc,
.error-detail {
  margin: 0;
  color: var(--text-2);
  font-size: var(--fs-body);
  line-height: var(--lh-body);
}

.error-detail {
  max-width: 100%;
  padding: var(--space-8) var(--space-12);
  border: 1px solid var(--border);
  border-radius: var(--radius-tag);
  background: var(--bg-subtle);
  color: var(--text-3);
  font-family: var(--font-mono);
  font-size: var(--fs-code);
  line-height: var(--lh-code);
  overflow-wrap: anywhere;
}

.error :deep(.btn--primary) {
  margin-top: var(--space-8);
}
</style>
