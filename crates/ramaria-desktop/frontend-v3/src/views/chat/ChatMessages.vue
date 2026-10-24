<script setup lang="ts">
/**
 * src/views/chat/ChatMessages.vue — 消息列表
 *
 * 职责:
 * - 渲染消息、日期分隔条、流式气泡、失败重试与加载 / 空 / 错误三态
 * - 新消息到达时贴底滚动；用户上翻后不打断阅读
 * - 通知定位时滚动到目标消息并短暂高亮
 *
 * 设计特点:
 * - 日期分隔条按本地自然日插入，第一条固定标注会话开始时间
 * - 流式渲染的真相源是累积全文，分隔符临时显示为换行
 * - 空态不提供按钮：输入区本身即可开始对话
 */
import { computed, nextTick, onMounted, ref, watch } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseBubble from '../../components/base/BaseBubble.vue'
import BaseEmptyState from '../../components/base/BaseEmptyState.vue'
import BaseSkeleton from '../../components/base/BaseSkeleton.vue'
import { canRecall } from '../../core/chat/recall'
import { dayDivider, dayKey, formatDateTime } from '../../core/chat/time'
import type { SessionMessage } from '../../core/api/dto'
import { stateCopy } from '../../core/stateCopy'
import { useNow } from '../../core/useNow'
import { useChatStore } from '../../stores/chat'
import { useSessionStore } from '../../stores/session'
import ChatAvatar from './ChatAvatar.vue'
import ChatMessage from './ChatMessage.vue'

const props = withDefaults(
  defineProps<{
    focusMessageId?: string | null
  }>(),
  { focusMessageId: null },
)

const emit = defineEmits<{
  recall: [message: SessionMessage]
  retry: []
  reload: []
}>()

const session = useSessionStore()
const chat = useChatStore()
const now = useNow(15_000)

const host = ref<HTMLElement | null>(null)
const pinned = ref(true)
const highlightId = ref<string | null>(null)

const emptyCopy = stateCopy('chat.empty.no-data')
const errorCopy = stateCopy('chat.error.load')

const loading = computed(() => session.loadingDetail && chat.messages.length === 0)
const failed = computed(() => session.error !== '' && chat.messages.length === 0)
const empty = computed(
  () => !loading.value && !failed.value && chat.messages.length === 0 && !chat.streaming,
)
const personaName = computed(() => session.activePersona?.name ?? '')
const personaInitial = computed(() => personaName.value.slice(0, 1) || '·')
/** 右侧「我」的昵称来源：与人格界面的 user 人格同源 */
const selfName = computed(() => session.userPersona?.name || '我')
/** 发言人视角：社交通道与导入历史按日志形态展示 */
const perspective = computed(
  () => session.activeChannel !== 'local' || session.imported,
)

type ListItem =
  | { kind: 'divider'; key: string; text: string }
  | { kind: 'message'; key: string; message: SessionMessage; recallable: boolean }

const items = computed<ListItem[]>(() => {
  const list: ListItem[] = []
  let lastDay = ''
  chat.messages.forEach((message, index) => {
    const day = dayKey(message.created_at)
    if (index === 0) {
      list.push({
        kind: 'divider',
        key: `start-${message.id}`,
        text: `会话开始 · ${formatDateTime(message.created_at)}`,
      })
    } else if (day !== lastDay) {
      list.push({
        kind: 'divider',
        key: `day-${day}-${index}`,
        text: dayDivider(message.created_at, now.value),
      })
    }
    lastDay = day
    list.push({
      kind: 'message',
      key: message.id,
      message,
      recallable: canRecall(message, { readonly: session.readonly, now: now.value }),
    })
  })
  return list
})

function onScroll(): void {
  const element = host.value
  if (!element) {
    return
  }
  pinned.value = element.scrollHeight - element.scrollTop - element.clientHeight < 80
}

async function scrollToBottom(): Promise<void> {
  await nextTick()
  const element = host.value
  if (element) {
    element.scrollTop = element.scrollHeight
  }
}

watch(
  () => chat.messages.length,
  () => {
    if (pinned.value) {
      void scrollToBottom()
    }
  },
)

watch(
  () => chat.streamingText,
  () => {
    if (pinned.value) {
      void scrollToBottom()
    }
  },
)

watch(
  () => session.activeId,
  () => {
    pinned.value = true
    void scrollToBottom()
  },
)

watch(
  () => props.focusMessageId,
  async (target) => {
    if (!target) {
      return
    }
    await nextTick()
    const nodes = host.value?.querySelectorAll<HTMLElement>('[data-message-id]')
    const hit = nodes ? Array.from(nodes).find((node) => node.dataset.messageId === target) : null
    if (!hit) {
      return
    }
    hit.scrollIntoView({ block: 'center' })
    highlightId.value = target
    window.setTimeout(() => {
      highlightId.value = null
    }, 2000)
  },
)

onMounted(() => {
  void scrollToBottom()
})
</script>

<template>
  <div ref="host" class="messages" role="log" aria-live="polite" @scroll="onScroll">
    <template v-if="loading">
      <div v-for="index in 3" :key="index" class="skeleton-row">
        <BaseSkeleton shape="bubble" />
      </div>
    </template>

    <div v-else-if="failed" class="state-block" role="alert">
      <p class="state-title">{{ errorCopy.title }}</p>
      <p class="state-desc">{{ session.error || errorCopy.description }}</p>
      <BaseButton variant="secondary" size="sm" @click="emit('reload')">
        {{ errorCopy.action ?? '重试' }}
      </BaseButton>
    </div>

    <BaseEmptyState
      v-else-if="empty"
      :title="emptyCopy.title"
      :description="emptyCopy.description"
      class="empty"
    >
      <template #mark>
        <span class="empty-initial">{{ personaInitial }}</span>
      </template>
    </BaseEmptyState>

    <template v-else>
      <template v-for="item in items" :key="item.key">
        <p v-if="item.kind === 'divider'" class="divider">{{ item.text }}</p>
        <ChatMessage
          v-else
          :message="item.message"
          :persona-name="personaName"
          :self-name="selfName"
          :perspective="perspective"
          :can-recall="item.recallable"
          :highlight="highlightId === item.message.id"
          @recall="emit('recall', $event)"
        />
      </template>

      <div v-if="chat.streaming" class="msg-row" data-role="assistant">
        <ChatAvatar :initial="personaInitial" />
        <div class="stack" data-role="assistant">
          <BaseBubble role="assistant" variant="streaming">
            <span class="bubble-text">{{ chat.streamingDisplay }}</span>
          </BaseBubble>
        </div>
      </div>

      <div v-else-if="chat.error && chat.failedText" class="msg-row" data-role="assistant">
        <ChatAvatar :initial="personaInitial" />
        <div class="stack" data-role="assistant">
          <BaseBubble role="assistant" variant="failed" @retry="emit('retry')">
            <span class="bubble-text">{{ chat.error }}</span>
          </BaseBubble>
        </div>
      </div>
    </template>
  </div>
</template>

<style scoped>
.messages {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: var(--space-14);
  min-height: 0;
  padding: var(--space-6) var(--space-6) var(--space-6) var(--space-4);
  overflow-y: auto;
  overscroll-behavior: contain;
  /* 顶部渐隐收在 6px 内边距内，避免吃掉「会话开始」提示；底部保留输入区过渡 */
  --messages-fade-top: var(--space-6);
  --messages-fade-bottom: var(--space-24);
  -webkit-mask-image: linear-gradient(
    180deg,
    transparent 0,
    #000 var(--messages-fade-top),
    #000 calc(100% - var(--messages-fade-bottom)),
    transparent 100%
  );
  mask-image: linear-gradient(
    180deg,
    transparent 0,
    #000 var(--messages-fade-top),
    #000 calc(100% - var(--messages-fade-bottom)),
    transparent 100%
  );
}

.messages::-webkit-scrollbar {
  width: 6px;
}

.messages::-webkit-scrollbar-thumb {
  border-radius: 999px;
  background: var(--border-strong);
}

.messages::-webkit-scrollbar-track {
  background: transparent;
}

.divider {
  align-self: center;
  padding: var(--space-4) var(--space-14);
  border: 1px solid var(--border);
  border-radius: var(--radius-tag);
  background: var(--bg-subtle);
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.msg-row {
  display: flex;
  align-items: flex-end;
  gap: var(--space-10);
}

.msg-row[data-role='assistant'] {
  justify-content: flex-start;
}

.stack {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
  max-width: 76%;
}

.stack :deep(.bubble) {
  max-width: 100%;
}

.bubble-text {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.skeleton-row {
  display: flex;
  padding: var(--space-4) 0;
}

.state-block {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: var(--space-8);
  margin: auto;
  padding: var(--space-24);
  border: 1px solid var(--danger-line);
  border-radius: var(--radius-card);
  background: var(--danger-surface);
}

.state-title {
  color: var(--danger);
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
}

.state-desc {
  color: var(--text-2);
  font-size: var(--fs-small);
}

.empty {
  margin: auto;
}

.empty-initial {
  display: grid;
  place-items: center;
  width: 100%;
  height: 100%;
}

@media (max-width: 720px) {
  .stack {
    max-width: 88%;
  }
}
</style>
