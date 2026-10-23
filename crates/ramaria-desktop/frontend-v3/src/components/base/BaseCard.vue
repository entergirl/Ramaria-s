<script setup lang="ts">
/**
 * src/components/base/BaseCard.vue — 卡片容器
 *
 * 职责:
 * - 承载信息单元，提供可选的可点击、选中、加载、错误、未保存、变暗六种标记
 *
 * 设计特点:
 * - 可点击卡才有 hover 与键盘激活，不可点击卡不响应指针
 * - 错误留在卡内，不切断其它卡片；变暗仅用于刷新态旧内容
 * - 圆角与描边来自令牌，卡片边界由描边与投影共同承担
 */

withDefaults(
  defineProps<{
    clickable?: boolean
    selected?: boolean
    state?: 'default' | 'loading' | 'error'
    dirty?: boolean
    dimmed?: boolean
    errorText?: string
  }>(),
  {
    clickable: false,
    selected: false,
    state: 'default',
    dirty: false,
    dimmed: false,
    errorText: '这张卡读取失败',
  },
)

const emit = defineEmits<{
  activate: []
}>()

function activate(): void {
  emit('activate')
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Enter' && event.key !== ' ') {
    return
  }
  event.preventDefault()
  activate()
}
</script>

<template>
  <div
    class="card"
    :class="{ 'card--clickable': clickable, 'card--dimmed': dimmed }"
    :role="clickable ? 'button' : undefined"
    :tabindex="clickable ? 0 : undefined"
    :data-state="state !== 'default' ? state : undefined"
    :data-dirty="dirty ? '' : undefined"
    :data-selected="selected ? '' : undefined"
    @click="clickable ? activate() : undefined"
    @keydown="clickable ? onKeydown($event) : undefined"
  >
    <p v-if="state === 'error'" class="card-error" role="alert">
      <slot name="error">{{ errorText }}</slot>
    </p>
    <span v-if="dirty" class="card-dirty">未保存</span>
    <slot />
  </div>
</template>

<style scoped>
.card {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: var(--space-12);
  padding: var(--pad-card);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-sm);
  transition:
    box-shadow var(--dur-fast) var(--ease-out),
    border-color var(--dur-fast) var(--ease-out),
    transform var(--dur-fast) var(--ease-out);
}

.card--clickable {
  cursor: pointer;
}

.card--clickable:hover {
  box-shadow: var(--shadow-md);
  transform: translateY(-1px);
}

.card--clickable:active {
  transform: translateY(0);
}

.card--clickable:focus-visible {
  outline: none;
  box-shadow: var(--shadow-md), var(--focus-ring);
}

.card[data-selected] {
  border-color: var(--brand);
}

.card[data-selected]:hover {
  border-color: var(--brand);
}

.card--dimmed {
  opacity: var(--opacity-dim);
}

.card[data-state='loading'] {
  cursor: progress;
}

.card-error {
  margin: 0;
  padding: var(--space-8) var(--space-10);
  border: 1px solid var(--danger-line);
  border-radius: var(--radius-tag);
  background: var(--danger-surface);
  color: var(--danger);
  font-size: var(--fs-small);
}

.card-dirty {
  position: absolute;
  top: var(--space-8);
  right: var(--space-12);
  color: var(--warn);
  font-size: var(--fs-caption);
}
</style>
