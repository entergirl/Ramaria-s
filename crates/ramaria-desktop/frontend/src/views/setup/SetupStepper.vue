<script setup lang="ts">
/**
 * src/views/setup/SetupStepper.vue — 配置向导步骤条
 *
 * 职责:
 * - 展示四步进度：已完成 / 当前 / 待开始，并给出可读的步骤标签
 *
 * 设计特点:
 * - 当前步用品牌底圆形，已完成步用成功底并显示勾选，待开始步为空心圆
 * - 窄窗只留圆点与连接线，隐藏文字标签，不换行
 */

defineProps<{
  current: number
  labels: readonly string[]
}>()
</script>

<template>
  <ol class="steps">
    <li
      v-for="(label, index) in labels"
      :key="label"
      class="step"
      :class="{ 'is-current': index + 1 === current, 'is-done': index + 1 < current }"
      :aria-current="index + 1 === current ? 'step' : undefined"
    >
      <span class="step-num" aria-hidden="true">{{ index + 1 < current ? '✓' : index + 1 }}</span>
      <span class="step-label">{{ label }}</span>
      <span
        v-if="index < labels.length - 1"
        class="step-line"
        :class="{ 'is-done': index + 1 < current }"
        aria-hidden="true"
      />
    </li>
  </ol>
</template>

<style scoped>
.steps {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--space-10);
  margin: 0;
  padding: 0;
  list-style: none;
}

.step {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--space-8);
  color: var(--text-3);
  font-size: var(--fs-small);
  font-weight: var(--fw-medium);
  white-space: nowrap;
}

.step-num {
  display: grid;
  place-items: center;
  flex: none;
  width: 24px;
  height: 24px;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-full);
  background: var(--bg-card);
  font-size: var(--fs-caption);
  font-weight: var(--fw-semibold);
  line-height: 1;
}

.step.is-current {
  color: var(--brand-text);
  font-weight: var(--fw-semibold);
}

.step.is-current .step-num {
  border-color: var(--brand);
  background: var(--brand);
  color: var(--btn-fg);
}

.step.is-done .step-num {
  border-color: var(--ok-line);
  background: var(--ok-surface);
  color: var(--ok);
}

.step.is-done {
  color: var(--text-2);
}

.step.is-current .step-label {
  color: var(--brand-text);
}

.step-line {
  width: var(--space-48);
  height: 1px;
  background: var(--border);
}

.step-line.is-done {
  background: var(--ok-line);
}

@media (max-width: 680px) {
  .step-label {
    display: none;
  }
}
</style>
