<script setup lang="ts">
/**
 * src/views/import/ImportStepper.vue — 导入向导步骤条
 *
 * 职责:
 * - 展示四步进度：已完成 / 当前 / 待开始，并给出可读步骤名
 *
 * 设计特点:
 * - 语义为有序列表，当前步带 aria-current="step"
 * - 整体水平居中，步间连线定宽，不随卡片宽度拉伸
 * - 已完成步显示勾选；窄窗只留圆点与连线，隐藏文字
 * - 纯展示组件，不响应点击（执行中不允许跳步）
 */

defineProps<{
  current: number
  labels: readonly string[]
}>()
</script>

<template>
  <ol class="steps" aria-label="导入步骤">
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
  gap: var(--space-8);
  margin: 0;
  padding: 0;
  list-style: none;
}

.step {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  color: var(--text-3);
  font-size: var(--fs-small);
  font-weight: var(--fw-medium);
  white-space: nowrap;
}

.step-num {
  display: grid;
  flex: none;
  width: 24px;
  height: 24px;
  place-items: center;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-full);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
  font-weight: var(--fw-bold);
}

.step.is-current {
  color: var(--text-1);
}

.step.is-current .step-num {
  border-color: var(--brand);
  background: var(--brand);
  color: var(--btn-fg);
}

.step.is-done {
  color: var(--text-2);
}

.step.is-done .step-num {
  border-color: var(--ok-line);
  background: var(--ok-surface);
  color: var(--ok);
}

.step-label {
  overflow: hidden;
  text-overflow: ellipsis;
}

.step-line {
  flex: none;
  width: var(--space-40);
  height: 1px;
  background: var(--border);
}

.step-line.is-done {
  background: var(--ok-line);
}

@media (max-width: 720px) {
  .step-label {
    display: none;
  }
}
</style>
