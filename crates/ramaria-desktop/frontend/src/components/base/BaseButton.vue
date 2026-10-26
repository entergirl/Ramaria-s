<script setup lang="ts">
/**
 * src/components/base/BaseButton.vue — 基础按钮
 *
 * 职责:
 * - 提供主 / 次 / 幽灵 / 危险四种形态与 md / sm 两档尺寸
 * - 承载加载态与禁用态，加载时宽度不跳变
 *
 * 设计特点:
 * - 形态走 class、异步态走 data-*、交互态走伪类，四类状态不混用
 * - 加载时原生 disabled，但不降透明度，只显示左侧旋转指示
 * - 图标按钮宽度等于高度，圆角仍为按钮圆角（不做正圆）
 */

withDefaults(
  defineProps<{
    variant?: 'primary' | 'secondary' | 'ghost' | 'danger'
    size?: 'md' | 'sm'
    loading?: boolean
    disabled?: boolean
    iconOnly?: boolean
    type?: 'button' | 'submit' | 'reset'
  }>(),
  {
    variant: 'secondary',
    size: 'md',
    loading: false,
    disabled: false,
    iconOnly: false,
    type: 'button',
  },
)
</script>

<template>
  <button
    :type="type"
    class="btn"
    :class="[`btn--${variant}`, `btn--${size}`, { 'btn--icon': iconOnly }]"
    :data-loading="loading ? '' : undefined"
    :disabled="disabled || loading"
  >
    <span v-if="loading" class="btn-spinner" aria-hidden="true" />
    <slot />
  </button>
</template>

<style scoped>
.btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: var(--space-8);
  height: 32px;
  padding: 0 var(--space-16);
  border: none;
  border-radius: var(--radius-btn);
  font-size: var(--fs-body);
  font-weight: var(--fw-semibold);
  line-height: 1;
  white-space: nowrap;
  cursor: pointer;
  transition:
    background-color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out);
}

.btn--sm {
  height: 26px;
  padding: 0 var(--space-12);
  border-radius: var(--radius-btn-sm);
  font-size: var(--fs-small);
}

.btn--icon {
  width: 32px;
  padding: 0;
}

.btn--icon.btn--sm {
  width: 26px;
}

.btn--primary {
  background: var(--btn-bg);
  color: var(--btn-fg);
  box-shadow: var(--btn-shadow);
}

.btn--primary:hover:not(:disabled) {
  background: var(--btn-bg-hover);
}

.btn--primary:active:not(:disabled) {
  background: var(--btn-bg-active);
  transform: translateY(1px);
}

.btn--secondary {
  background: var(--btn-secondary-bg);
  color: var(--btn-secondary-fg);
  border: 1px solid var(--btn-secondary-border);
  box-shadow: var(--shadow-xs);
}

.btn--secondary:hover:not(:disabled) {
  background: var(--bg-subtle);
}

.btn--secondary:active:not(:disabled) {
  transform: translateY(1px);
}

.btn--ghost {
  background: transparent;
  color: var(--text-2);
}

.btn--ghost:hover:not(:disabled) {
  background: var(--bg-subtle);
}

.btn--ghost:active:not(:disabled) {
  transform: translateY(1px);
}

.btn--danger {
  background: var(--btn-danger-bg);
  color: var(--btn-danger-fg);
}

.btn--danger:hover:not(:disabled) {
  background: var(--danger-line);
}

.btn:focus-visible {
  outline: none;
  box-shadow: var(--btn-shadow), var(--focus-ring);
}

.btn--secondary:focus-visible,
.btn--ghost:focus-visible,
.btn--danger:focus-visible {
  box-shadow: var(--focus-ring);
}

.btn:disabled {
  opacity: var(--opacity-disabled);
  box-shadow: none;
  cursor: default;
}

.btn[data-loading] {
  opacity: 1;
  cursor: progress;
}

.btn-spinner {
  width: 14px;
  height: 14px;
  border: 2px solid currentColor;
  border-right-color: transparent;
  border-radius: var(--radius-full);
  animation: btn-spin var(--dur-loop-spin) linear infinite;
}

@keyframes btn-spin {
  to {
    transform: rotate(360deg);
  }
}

@media (prefers-reduced-motion: reduce) {
  .btn-spinner {
    animation: none;
  }
}
</style>
