<script setup lang="ts">
/**
 * src/views/chat/ChatHeader.vue — 对话头部
 *
 * 职责:
 * - 展示当前人格头像、名称、来源标签与对话状态
 * - 提供会话卡片开关与「保存对话」动作
 *
 * 设计特点:
 * - 头像即会话卡片入口，展开状态经 `aria-expanded` 透出
 * - 头像优先用已保存图片，缺失或不可读时回退名称首字
 * - 状态点与文案同源：流式、只读、已结束、正常四种
 * - 保存键只在可保存时出现，不出现灰掉的无效键
 */
import { computed, ref, watch } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import { sessionTags } from '../../core/chat/sessionList'
import { useChatStore } from '../../stores/chat'
import { useSessionStore } from '../../stores/session'

const props = withDefaults(
  defineProps<{
    sessionsOpen: boolean
    saving?: boolean
  }>(),
  { saving: false },
)

defineEmits<{
  'toggle-sessions': []
  save: []
}>()

const session = useSessionStore()
const chat = useChatStore()

const persona = computed(() => session.activePersona)
const name = computed(() => persona.value?.name || '未命名人格')
const initial = computed(() => name.value.slice(0, 1))

const previewFailed = ref(false)
const avatarSrc = computed(() => {
  const value = persona.value?.avatar?.trim() ?? ''
  return value !== '' && !previewFailed.value ? value : ''
})

watch(
  () => persona.value?.avatar,
  () => {
    previewFailed.value = false
  },
)

const tags = computed(() => sessionTags(session.active?.channel ?? 'local', persona.value))

const statusText = computed(() => {
  if (chat.streaming) {
    return '正在生成…'
  }
  if (session.readonly) {
    return session.imported ? '导入消息 · 只读' : '只读会话'
  }
  if (session.closed) {
    return '已结束 · 输入将开始新会话'
  }
  return session.activeId ? '对话中' : '新会话'
})

const canSave = computed(
  () =>
    session.activeId !== null &&
    !session.closed &&
    !session.readonly &&
    !chat.streaming &&
    !props.saving,
)
</script>

<template>
  <section class="chat-head">
    <button
      type="button"
      class="avatar"
      :aria-expanded="sessionsOpen ? 'true' : 'false'"
      aria-controls="chat-sessions"
      aria-label="打开会话列表"
      @click="$emit('toggle-sessions')"
    >
      <span class="avatar-ring" aria-hidden="true" />
      <img
        v-if="avatarSrc"
        class="avatar-img"
        :src="avatarSrc"
        alt=""
        @error="previewFailed = true"
      />
      <span v-else class="avatar-text">{{ initial }}</span>
    </button>

    <div class="head-info">
      <div class="head-name">
        <span class="name">{{ name }}</span>
        <span v-if="tags.length" class="tags">
          <BaseTag v-for="tag in tags" :key="tag.label" :tone="tag.tone">{{ tag.label }}</BaseTag>
        </span>
      </div>
      <div class="head-status" :data-streaming="chat.streaming ? '' : undefined">
        <span class="dot" aria-hidden="true" />
        <span>{{ statusText }}</span>
      </div>
    </div>

    <div class="head-actions">
      <BaseButton
        v-if="canSave"
        variant="secondary"
        size="sm"
        :loading="saving"
        @click="$emit('save')"
      >
        保存对话
      </BaseButton>
    </div>
  </section>
</template>

<style scoped>
.chat-head {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--space-14);
  padding: var(--space-14) var(--space-20);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-md);
}

.avatar {
  position: relative;
  display: grid;
  flex: none;
  place-items: center;
  width: 46px;
  height: 46px;
  padding: 0;
  border: none;
  background: transparent;
  color: var(--brand-text);
  font-size: var(--fs-h2);
  font-weight: var(--fw-bold);
  line-height: 1;
  cursor: pointer;
}

/* 背景圆形单独承接悬停投影，文字不参与位移与阴影 */
.avatar-ring {
  position: absolute;
  inset: 0;
  border: 1px solid var(--brand-line);
  border-radius: var(--radius-full);
  background: var(--brand-surface);
  transition:
    border-color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out);
}

.avatar-text {
  position: relative;
  z-index: 1;
  line-height: 1;
  /* 汉字墨迹偏左上，按视觉重心做亚像素级光学微调 */
  transform: translate(0.04em, 0.02em);
}

/* 图片头像铺满圆形，不叠加文字头像的内描边 */
.avatar-img {
  position: relative;
  z-index: 1;
  width: 100%;
  height: 100%;
  border-radius: var(--radius-full);
  object-fit: cover;
}

.avatar:hover .avatar-ring {
  box-shadow: var(--shadow-sm);
}

.avatar:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.avatar[aria-expanded='true'] .avatar-ring {
  border-color: var(--brand);
  box-shadow: 0 0 0 3px var(--brand-surface);
}

.head-info {
  flex: 1;
  min-width: 0;
}

.head-name {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
}

.name {
  font-size: var(--fs-h3);
  font-weight: var(--fw-bold);
  line-height: var(--lh-h3);
}

.tags {
  display: inline-flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-6);
}

.head-status {
  display: flex;
  align-items: center;
  gap: var(--space-6);
  margin-top: var(--space-4);
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.dot {
  width: 7px;
  height: 7px;
  border-radius: var(--radius-full);
  background: var(--ok);
  box-shadow: 0 0 0 3px var(--ok-surface);
}

.head-status[data-streaming] .dot {
  background: var(--brand);
  box-shadow: 0 0 0 3px var(--brand-surface);
  animation: status-pulse 1200ms var(--ease-in-out) infinite;
}

.head-actions {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--space-8);
}

@keyframes status-pulse {
  50% {
    opacity: 0.35;
  }
}

@media (prefers-reduced-motion: reduce) {
  .head-status[data-streaming] .dot {
    animation: none;
  }
}

@media (max-width: 720px) {
  .head-actions {
    display: none;
  }
}
</style>
