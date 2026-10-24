<script setup lang="ts">
/**
 * src/views/chat/ChatMessage.vue — 单条消息
 *
 * 职责:
 * - 渲染一条消息的正文：`||` 拆条、图片段、@ 提及、换行保留
 * - 表达群聊发言人、主动标识、撤回系统条与撤回动作
 *
 * 设计特点:
 * - 撤回不保留原气泡，改为居中系统条；正文仍留在库里，只做展示替换
 * - 助手消息常驻左侧头像；单聊用户消息常驻右侧「我」头像，日志视角仍用发言人头像
 * - 用户头像的昵称与人格界面的 user 人格同源，改名后自动跟随
 * - 图片段序号以整条消息为口径，跨气泡连续累计，与后端读取入参一致
 * - 纯文本渲染，不做 Markdown 解析；换行由样式保留
 * - 撤回动作只在可撤回时渲染，避免出现点了会失败的入口
 */
import { computed } from 'vue'

import BaseBubble from '../../components/base/BaseBubble.vue'
import { splitBubbles } from '../../core/chat/bubble'
import { countImageSegments, splitImageSegments } from '../../core/chat/imageText'
import { splitMentions } from '../../core/chat/mention'
import { recallLabel } from '../../core/chat/recall'
import { formatClock, formatDateTime } from '../../core/chat/time'
import type { SessionMessage } from '../../core/api/dto'
import ChatAvatar from './ChatAvatar.vue'
import ChatImage from './ChatImage.vue'

const props = withDefaults(
  defineProps<{
    message: SessionMessage
    personaName?: string
    /** 单聊用户侧「我」的昵称（与人格界面同源） */
    selfName?: string
    /** 发言人视角：社交通道与导入历史按日志形态展示 */
    perspective?: boolean
    canRecall?: boolean
    /** 通知定位后的高亮 */
    highlight?: boolean
  }>(),
  { personaName: '', selfName: '我', perspective: false, canRecall: false, highlight: false },
)

defineEmits<{
  recall: [message: SessionMessage]
}>()

const role = computed<'user' | 'assistant' | 'system'>(() => {
  if (props.message.role === 'user' || props.message.role === 'assistant') {
    return props.message.role
  }
  return 'system'
})

const recalled = computed(() => props.message.recalled_at !== null)
const recallText = computed(() => recallLabel(props.message, props.personaName))

/** 展示分段：助手回复按分隔符拆条，其余整段单泡 */
const bubbles = computed(() => {
  if (role.value !== 'assistant') {
    return [props.message.content]
  }
  const segments = splitBubbles(props.message.content)
  return segments.length > 0 ? segments : [props.message.content]
})

const speakerName = computed(() => props.message.sender_name || props.personaName)
const showSpeaker = computed(() => props.message.sender_name !== null)
/** 本地会话的用户侧：右侧「我」，日志视角仍按发言人展示 */
const selfSide = computed(() => role.value === 'user' && !props.perspective)
const showAvatar = computed(() => role.value === 'assistant' || selfSide.value || showSpeaker.value)
const avatarName = computed(() => (selfSide.value ? props.selfName : speakerName.value))
const avatarInitial = computed(() => (avatarName.value || '我').slice(0, 1))
const avatarTone = computed(() => (role.value === 'user' ? 'accent' : 'brand'))

const clock = computed(() => formatClock(props.message.created_at))
const fullTime = computed(() => formatDateTime(props.message.created_at))

/** 第 index 条气泡之前已出现的图片数量（跨气泡累计序号） */
function imageBase(index: number): number {
  let base = 0
  for (let cursor = 0; cursor < index; cursor += 1) {
    base += countImageSegments(bubbles.value[cursor])
  }
  return base
}

function segmentsOf(text: string) {
  return splitImageSegments(text)
}

function inlineOf(text: string) {
  return splitMentions(text, props.perspective)
}
</script>

<template>
  <div v-if="recalled" class="msg-recalled">
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M3 7v6h6" />
      <path d="M21 17a9 9 0 0 0-9-9 9 9 0 0 0-6.36 2.64L3 13" />
    </svg>
    <span>{{ recallText }}</span>
  </div>

  <div
    v-else
    class="msg-row"
    :data-role="role"
    :data-perspective="perspective ? 'log' : undefined"
    :data-message-id="message.id"
    :data-recall="canRecall ? '' : undefined"
    :data-highlight="highlight ? '' : undefined"
  >
    <ChatAvatar
      v-if="showAvatar"
      :initial="avatarInitial"
      :tone="avatarTone"
      :title="avatarName || undefined"
    />

    <div class="stack" :data-role="role">
      <p v-if="showSpeaker" class="speaker-line">
        <span class="speaker-name">{{ speakerName }}</span>
        <time class="msg-time" :title="fullTime">{{ clock }}</time>
      </p>
      <time v-else-if="role !== 'system'" class="msg-time alone" :title="fullTime">
        {{ clock }}
      </time>

      <BaseBubble v-for="(bubble, bubbleIndex) in bubbles" :key="bubbleIndex" :role="role">
        <span
          v-if="message.is_proactive && bubbleIndex === 0"
          class="proactive-tag"
        >主动消息 · {{ clock }}</span>

        <template v-for="(segment, segmentIndex) in segmentsOf(bubble)" :key="segmentIndex">
          <ChatImage
            v-if="segment.type === 'image' && (segment.hash || segment.desc)"
            :message-id="message.id"
            :index="imageBase(bubbleIndex) + segment.index"
            :desc="segment.desc"
          />
          <span v-else-if="segment.type === 'image'" class="image-placeholder">
            <span class="placeholder-icon" aria-hidden="true">🖼</span>
            <span>图片不可读取</span>
          </span>
          <span v-else class="bubble-text">
            <template v-for="(part, partIndex) in inlineOf(segment.value)" :key="partIndex">
              <span v-if="part.type === 'mention'" class="mention">{{ part.value }}</span>
              <template v-else>{{ part.value }}</template>
            </template>
          </span>
        </template>
      </BaseBubble>

      <button
        v-if="canRecall"
        type="button"
        class="recall-btn"
        aria-label="撤回这条消息"
        @click="$emit('recall', message)"
      >
        撤回
      </button>
    </div>
  </div>
</template>

<style scoped>
.msg-row {
  display: flex;
  align-items: flex-end;
  gap: var(--space-10);
}

.msg-row[data-role='user'] {
  justify-content: flex-end;
}

.msg-row[data-role='assistant'],
.msg-row[data-perspective='log'][data-role='user'] {
  justify-content: flex-start;
}

/* 单聊把「我」头像放到气泡右侧 */
.msg-row[data-role='user']:not([data-perspective]) .chat-avatar {
  order: 1;
}

/* 撤回键占住气泡下方一行，头像随之上移，保持与气泡底对齐 */
.msg-row[data-role='user']:not([data-perspective])[data-recall] .chat-avatar {
  margin-bottom: calc(var(--fs-caption) * var(--lh-caption) + var(--space-4));
}

.stack {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
  max-width: 76%;
}

/* 气泡宽度由消息列决定，避免与气泡自身的宽度上限叠乘 */
.stack :deep(.bubble) {
  max-width: 100%;
}

.stack[data-role='assistant'] {
  align-items: flex-start;
}

.stack[data-role='user'] {
  align-items: flex-end;
}

.msg-row[data-perspective='log'] .stack[data-role='user'] {
  align-items: flex-start;
}

.speaker-line {
  display: flex;
  align-items: baseline;
  gap: var(--space-6);
  padding-left: var(--space-4);
}

.speaker-name {
  color: var(--text-3);
  font-size: var(--fs-caption);
  font-weight: var(--fw-semibold);
}

.msg-time {
  color: var(--text-3);
  font-size: var(--fs-caption);
  font-variant-numeric: tabular-nums;
}

.msg-time.alone {
  padding: 0 var(--space-4);
  opacity: 0;
  transition: opacity var(--dur-fast) var(--ease-out);
}

.msg-row:hover .msg-time.alone,
.msg-row:focus-within .msg-time.alone {
  opacity: 1;
}

.bubble-text {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.mention {
  padding: 0 var(--space-4);
  border-radius: var(--radius-tag);
  background: var(--accent-surface);
  color: var(--accent-text);
  font-weight: var(--fw-bold);
}

.proactive-tag {
  display: block;
  margin-bottom: var(--space-6);
  color: var(--brand-text);
  font-size: var(--fs-caption);
  font-weight: var(--fw-semibold);
}

.image-placeholder {
  display: inline-flex;
  align-items: center;
  gap: var(--space-6);
  padding: var(--space-6) var(--space-10);
  border: 1px dashed var(--border-strong);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
  color: var(--text-3);
  font-size: var(--fs-small);
}

.placeholder-icon {
  flex: none;
}

.msg-row[data-highlight] :deep(.bubble-body) {
  box-shadow: 0 0 0 2px var(--brand-line);
  animation: highlight-in var(--dur-slow) var(--ease-out);
}

@keyframes highlight-in {
  from {
    box-shadow: 0 0 0 6px var(--brand-surface);
  }
}

.msg-recalled {
  display: flex;
  align-items: center;
  align-self: center;
  gap: var(--space-6);
  padding: var(--space-4) var(--space-12);
  border: 1px dashed var(--border-strong);
  border-radius: var(--radius-tag);
  background: var(--bg-subtle);
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.msg-recalled svg {
  width: 12px;
  height: 12px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.9;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.recall-btn {
  padding: 0 var(--space-6);
  border: none;
  border-radius: var(--radius-tag);
  background: transparent;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: var(--lh-caption);
  cursor: pointer;
  opacity: 0;
  transition: opacity var(--dur-fast) var(--ease-out);
}

.msg-row:hover .recall-btn,
.recall-btn:focus-visible {
  opacity: 1;
}

.recall-btn:hover {
  color: var(--danger);
  background: var(--danger-surface);
}

.recall-btn:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

@media (max-width: 720px) {
  .stack {
    max-width: 88%;
  }
}

@media (prefers-reduced-motion: reduce) {
  .msg-time.alone,
  .recall-btn {
    transition: none;
  }
}
</style>
