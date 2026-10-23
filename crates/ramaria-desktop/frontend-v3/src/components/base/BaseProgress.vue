<script setup lang="ts">
/**
 * src/components/base/BaseProgress.vue — 进度
 *
 * 职责:
 * - 表达确定进度（0~100）与不定进度，承载成功 / 失败 / 超时终态
 *
 * 设计特点:
 * - 确定进度用原生 progress 元素，宽度由浏览器按值绘制，不写内联样式
 * - 任何形态都带阶段文案或数值，不出现无说明的进度条
 * - 终态只改颜色与文案，收起与转内联由调用方决定
 */
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    value?: number
    label?: string
    state?: 'running' | 'success' | 'failed' | 'timeout'
  }>(),
  {
    value: undefined,
    label: '',
    state: 'running',
  },
)

const determinate = computed(() => typeof props.value === 'number')

const percentText = computed(() => (determinate.value ? `${Math.round(props.value as number)}%` : ''))

const defaultLabel = computed(() => {
  if (props.label) {
    return props.label
  }
  return determinate.value ? '进度' : '处理中'
})
</script>

<template>
  <div class="progress" :data-state="state">
    <div class="progress-head">
      <span class="progress-label">{{ defaultLabel }}</span>
      <span v-if="determinate" class="progress-value">{{ percentText }}</span>
    </div>
    <progress v-if="determinate" class="progress-bar" :value="value" max="100" />
    <div v-else class="progress-track" role="progressbar" aria-valuetext="进行中">
      <span class="progress-indeterminate" />
    </div>
  </div>
</template>

<style scoped>
.progress {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
  width: 100%;
}

.progress-head {
  display: flex;
  align-items: baseline;
  gap: var(--space-8);
}

.progress-label {
  flex: 1;
  color: var(--text-2);
  font-size: var(--fs-small);
}

.progress-value {
  color: var(--text-1);
  font-family: var(--font-mono);
  font-size: var(--fs-code);
}

.progress-bar {
  display: block;
  width: 100%;
  height: 4px;
  border: none;
  border-radius: var(--radius-full);
  appearance: none;
}

.progress-bar::-webkit-progress-bar {
  background: var(--bg-sunken);
  border-radius: var(--radius-full);
}

.progress-bar::-webkit-progress-value {
  background: var(--brand);
  border-radius: var(--radius-full);
  transition: width var(--dur-base) var(--ease-out);
}

.progress[data-state='success'] .progress-bar::-webkit-progress-value {
  background: var(--ok);
}

.progress[data-state='failed'] .progress-bar::-webkit-progress-value,
.progress[data-state='timeout'] .progress-bar::-webkit-progress-value {
  background: var(--danger);
}

.progress[data-state='success'] .progress-label {
  color: var(--ok);
}

.progress[data-state='failed'] .progress-label,
.progress[data-state='timeout'] .progress-label {
  color: var(--danger);
}

.progress-track {
  position: relative;
  width: 100%;
  height: 4px;
  overflow: hidden;
  border-radius: var(--radius-full);
  background: var(--bg-sunken);
}

.progress-indeterminate {
  position: absolute;
  inset: 0 auto 0 0;
  width: 40%;
  border-radius: var(--radius-full);
  background: var(--brand);
  animation: progress-slide 1200ms var(--ease-in-out) infinite;
}

@keyframes progress-slide {
  from {
    transform: translateX(-100%);
  }

  to {
    transform: translateX(250%);
  }
}

@media (prefers-reduced-motion: reduce) {
  .progress-indeterminate {
    animation: none;
    width: 100%;
    opacity: 0.4;
  }
}
</style>
