<script setup lang="ts">
/**
 * src/views/chat/ChatAvatar.vue — 文字头像
 *
 * 职责:
 * - 以首字呈现人格与发言人身份，供消息行、会话行复用
 *
 * 设计特点:
 * - 只承载文字头像；导入图片头像另行渲染，不带本组件的描边
 * - 描边走内描边，不改变 34px 占位；默认品牌色，用户与只读侧用辅助色
 * - 尺寸固定，调用方只决定色相与是否关闭描边
 */

withDefaults(
  defineProps<{
    initial: string
    tone?: 'brand' | 'accent'
    ring?: boolean
  }>(),
  { tone: 'brand', ring: true },
)
</script>

<template>
  <span class="chat-avatar" :class="[`tone-${tone}`, { 'has-ring': ring }]" aria-hidden="true">
    {{ initial }}
  </span>
</template>

<style scoped>
.chat-avatar {
  display: grid;
  flex: none;
  place-items: center;
  width: 34px;
  height: 34px;
  border-radius: var(--radius-full);
  background: var(--brand-surface);
  color: var(--brand-text);
  font-size: var(--fs-h3);
  font-weight: var(--fw-bold);
}

.chat-avatar.tone-accent {
  background: var(--accent-surface);
  color: var(--accent-text);
}

.chat-avatar.has-ring {
  box-shadow: inset 0 0 0 1px var(--brand-line);
}

.chat-avatar.tone-accent.has-ring {
  box-shadow: inset 0 0 0 1px var(--accent-line);
}
</style>
