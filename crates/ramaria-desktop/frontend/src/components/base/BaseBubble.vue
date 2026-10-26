<script setup lang="ts">
/**
 * src/components/base/BaseBubble.vue — 消息气泡
 *
 * 职责:
 * - 承载用户 / 助手 / 系统三种发言方向的正文，并表达流式、撤回、主动、失败、群聊五种标记
 *
 * 设计特点:
 * - 撤回不保留原气泡，改为居中系统条；失败不改变对齐，重试入口在气泡下方
 * - 群聊发言人区独立于正文，头像与名字只在有发言人时出现
 * - 流式光标由样式提供，正文真相源由调用方累积后传入
 */

import BaseTag from './BaseTag.vue'

withDefaults(
  defineProps<{
    role: 'user' | 'assistant' | 'system'
    variant?: 'normal' | 'streaming' | 'recalled' | 'proactive' | 'failed'
    name?: string
    avatar?: string
    tone?: 'default' | 'accent'
  }>(),
  {
    variant: 'normal',
    name: '',
    avatar: '',
    tone: 'default',
  },
)

defineEmits<{
  retry: []
}>()
</script>

<template>
  <article class="bubble" :data-role="role" :data-variant="variant">
    <p v-if="variant === 'recalled'" class="bubble-recalled">消息已撤回</p>
    <template v-else>
      <header v-if="name" class="bubble-head">
        <span class="bubble-avatar" :class="{ 'is-accent': tone === 'accent' }">
          {{ avatar || name.slice(0, 1) }}
        </span>
        <span class="bubble-name">{{ name }}</span>
        <BaseTag v-if="variant === 'proactive'" tone="brand">主动</BaseTag>
      </header>
      <div class="bubble-body">
        <slot />
      </div>
      <footer v-if="variant === 'failed'" class="bubble-foot">
        <button type="button" class="bubble-retry" @click="$emit('retry')">重试</button>
      </footer>
    </template>
  </article>
</template>

<style scoped>
.bubble {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
  max-width: 76%;
}

.bubble[data-role='user'] {
  align-self: flex-end;
  align-items: flex-end;
}

.bubble[data-role='assistant'] {
  align-self: flex-start;
  align-items: flex-start;
}

.bubble[data-role='system'] {
  align-self: center;
  align-items: center;
  max-width: 100%;
}

.bubble-head {
  display: flex;
  align-items: center;
  gap: var(--space-6);
}

.bubble-avatar {
  display: grid;
  place-items: center;
  width: 20px;
  height: 20px;
  border-radius: var(--radius-full);
  background: var(--brand-surface);
  color: var(--brand-text);
  font-size: var(--fs-caption);
  font-weight: var(--fw-semibold);
}

.bubble-avatar.is-accent {
  background: var(--accent-surface);
  color: var(--accent-text);
}

.bubble-name {
  color: var(--text-3);
  font-size: var(--fs-small);
}

.bubble-body {
  padding: var(--space-10) var(--space-14);
  border-radius: var(--radius-bubble);
  background: var(--bg-card);
  border: 1px solid var(--border);
  box-shadow: var(--shadow-sm);
  color: var(--text-1);
  font-size: var(--fs-body);
  line-height: var(--lh-body);
  overflow-wrap: anywhere;
}

.bubble[data-role='user'] .bubble-body {
  background: var(--bubble-user-bg);
  border-color: var(--bubble-user-line);
  border-bottom-right-radius: var(--radius-bubble-tail);
  color: var(--text-1);
}

.bubble[data-role='assistant'] .bubble-body {
  border-bottom-left-radius: var(--radius-bubble-tail);
}

.bubble[data-role='system'] .bubble-body {
  background: var(--bg-subtle);
  border-color: var(--border);
  box-shadow: none;
  color: var(--text-3);
  font-size: var(--fs-small);
}

.bubble[data-variant='streaming'] .bubble-body::after {
  content: '';
  display: inline-block;
  width: 6px;
  height: 12px;
  margin-left: var(--space-4);
  background: var(--brand);
  vertical-align: -1px;
  animation: bubble-caret var(--dur-loop-caret) steps(2, start) infinite;
}

.bubble[data-variant='failed'] .bubble-body {
  border-color: var(--danger-line);
  background: var(--danger-surface);
}

/* 撤回系统条做成胶囊：半径取条高的一半，宽度仍随文字 */
.bubble-recalled {
  --recalled-height: 24px;

  display: inline-flex;
  align-items: center;
  min-height: var(--recalled-height);
  margin: 0;
  padding: 0 var(--space-12);
  border-radius: calc(var(--recalled-height) / 2);
  background: var(--bg-sunken);
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.bubble-foot {
  display: flex;
  gap: var(--space-8);
}

.bubble-retry {
  padding: 0;
  border: none;
  background: transparent;
  color: var(--brand-text);
  font-size: var(--fs-small);
  cursor: pointer;
}

.bubble-retry:hover {
  text-decoration: underline;
}

.bubble-retry:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
  border-radius: var(--radius-check);
}

@keyframes bubble-caret {
  50% {
    opacity: 0;
  }
}

@media (prefers-reduced-motion: reduce) {
  .bubble[data-variant='streaming'] .bubble-body::after {
    animation: none;
  }
}
</style>
