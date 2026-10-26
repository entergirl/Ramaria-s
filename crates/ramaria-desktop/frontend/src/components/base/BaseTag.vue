<script setup lang="ts">
/**
 * src/components/base/BaseTag.vue — 标签 / 徽章
 *
 * 职责:
 * - 表达状态、分类与筛选项，可选移除
 *
 * 设计特点:
 * - 圆角 8，不做全圆角，与按钮视觉语言区分
 * - 文字长度由调用方约束在 6 字以内，超出以省略号截断
 * - 除筛选与移除不承载其它操作
 */

withDefaults(
  defineProps<{
    tone?: 'neutral' | 'brand' | 'accent' | 'ok' | 'warn' | 'danger'
    selected?: boolean
    removable?: boolean
  }>(),
  {
    tone: 'neutral',
    selected: false,
    removable: false,
  },
)

defineEmits<{
  remove: []
}>()
</script>

<template>
  <span
    class="tag"
    :class="[`tag--${tone}`, { 'is-selected': selected }]"
    :data-selected="selected ? '' : undefined"
  >
    <span class="tag-text"><slot /></span>
    <button v-if="removable" type="button" class="tag-remove" aria-label="移除" @click="$emit('remove')">
      <svg viewBox="0 0 8 8" aria-hidden="true">
        <path d="M1.5 1.5l5 5M6.5 1.5l-5 5" />
      </svg>
    </button>
  </span>
</template>

<style scoped>
.tag {
  display: inline-flex;
  align-items: center;
  gap: var(--space-4);
  max-width: 100%;
  height: 20px;
  padding: 0 var(--space-8);
  border: 1px solid transparent;
  border-radius: var(--radius-tag);
  font-size: var(--fs-label);
  font-weight: var(--fw-semibold);
  line-height: 1;
}

.tag-text {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tag--neutral {
  background: var(--bg-subtle);
  border-color: var(--border);
  color: var(--text-2);
}

.tag--brand {
  background: var(--brand-surface);
  border-color: var(--brand-line);
  color: var(--brand-text);
}

.tag--accent {
  background: var(--accent-surface);
  border-color: var(--accent-line);
  color: var(--accent-text);
}

.tag--ok {
  background: var(--ok-surface);
  border-color: var(--ok-line);
  color: var(--ok);
}

.tag--warn {
  background: var(--warn-surface);
  border-color: var(--warn-line);
  color: var(--warn);
}

.tag--danger {
  background: var(--danger-surface);
  border-color: var(--danger-line);
  color: var(--danger);
}

.tag.is-selected {
  border-color: var(--brand);
  color: var(--brand-text);
}

.tag-remove {
  display: grid;
  place-items: center;
  width: 12px;
  height: 12px;
  padding: 0;
  border: none;
  border-radius: var(--radius-full);
  background: transparent;
  color: currentColor;
  cursor: pointer;
  opacity: 0.7;
}

.tag-remove svg {
  width: 8px;
  height: 8px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.4;
  stroke-linecap: round;
}

.tag-remove:hover {
  opacity: 1;
  background: var(--bg-sunken);
}

.tag-remove:active {
  transform: scale(0.92);
}

.tag-remove:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}
</style>
