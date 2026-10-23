<script setup lang="ts">
/**
 * src/components/base/BaseToast.vue — 轻提示条目
 *
 * 职责:
 * - 呈现单条提示的类型、正文与可选操作
 *
 * 设计特点:
 * - 类型走 data-tone，图标与颜色同时表达，不只靠颜色
 * - 正文最多两行，超出以省略号截断
 * - 关闭键常驻，错误类由队列决定不自动消失
 */

withDefaults(
  defineProps<{
    tone?: 'info' | 'success' | 'warn' | 'danger'
    message: string
    actionLabel?: string
  }>(),
  {
    tone: 'info',
    actionLabel: '',
  },
)

defineEmits<{
  activate: []
  close: []
}>()
</script>

<template>
  <div class="toast" :data-tone="tone" role="status">
    <span class="toast-mark" aria-hidden="true">
      <svg v-if="tone === 'success'" viewBox="0 0 12 12">
        <path d="M2.5 6.2 4.8 8.5 9.5 3.8" />
      </svg>
      <svg v-else-if="tone === 'warn' || tone === 'danger'" viewBox="0 0 12 12">
        <path d="M6 3v4M6 9h.01" />
      </svg>
      <svg v-else viewBox="0 0 12 12">
        <path d="M6 5.2v4M6 3h.01" />
      </svg>
    </span>
    <p class="toast-message">{{ message }}</p>
    <button v-if="actionLabel" type="button" class="toast-action" @click="$emit('activate')">
      {{ actionLabel }}
    </button>
    <button type="button" class="toast-close" aria-label="关闭提示" @click="$emit('close')">
      <svg viewBox="0 0 12 12" aria-hidden="true">
        <path d="M2.5 2.5l7 7M9.5 2.5l-7 7" />
      </svg>
    </button>
  </div>
</template>

<style scoped>
.toast {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  width: min(360px, calc(100vw - var(--space-32)));
  padding: var(--space-10) var(--space-12);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  box-shadow: var(--shadow-md);
  color: var(--text-1);
  font-size: var(--fs-small);
}

.toast-mark {
  display: grid;
  place-items: center;
  flex: none;
  width: 16px;
  height: 16px;
  border-radius: var(--radius-full);
  background: var(--bg-subtle);
  color: var(--text-2);
}

.toast-mark svg {
  width: 10px;
  height: 10px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.toast[data-tone='success'] .toast-mark {
  background: var(--ok-surface);
  color: var(--ok);
}

.toast[data-tone='warn'] .toast-mark {
  background: var(--warn-surface);
  color: var(--warn);
}

.toast[data-tone='danger'] .toast-mark {
  background: var(--danger-surface);
  color: var(--danger);
}

.toast[data-tone='danger'] {
  border-color: var(--danger-line);
}

.toast-message {
  flex: 1;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  margin: 0;
  line-height: 1.5;
}

.toast-action {
  flex: none;
  padding: 0;
  border: none;
  background: transparent;
  color: var(--brand-text);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
  cursor: pointer;
}

.toast-action:hover {
  text-decoration: underline;
}

.toast-action:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
  border-radius: var(--radius-check);
}

.toast-close {
  display: grid;
  place-items: center;
  flex: none;
  width: 18px;
  height: 18px;
  padding: 0;
  border: none;
  border-radius: var(--radius-check);
  background: transparent;
  color: var(--text-3);
  cursor: pointer;
}

.toast-close svg {
  width: 9px;
  height: 9px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.4;
  stroke-linecap: round;
}

.toast-close:hover {
  color: var(--text-1);
  background: var(--bg-subtle);
}

.toast-close:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}
</style>
