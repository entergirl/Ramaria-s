<script setup lang="ts">
/**
 * src/views/chat/index.vue — 对话视图入口
 *
 * 职责:
 * - 编排头部、横幅、消息列、输入区与会话卡片
 * - 承载撤回确认与隐私确认两个模态
 * - 派生视图六态标记（主态 / 降级 / 受限 / 忙碌）
 *
 * 设计特点:
 * - 数据与动作在 composable 与 store 中，本组件只做布局与事件接线
 * - 视窗上限 800×600 居中漂浮；小窗铺满可用高度，圆键由用户拖动避让
 * - 会话卡片展开时消息列与横幅右移，头部与输入区保持全宽
 * - 只读会话不渲染输入区，底部条只表达状态、不提供动作
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseCheckbox from '../../components/base/BaseCheckbox.vue'
import BaseModal from '../../components/base/BaseModal.vue'
import BaseWorkbench from '../../components/base/BaseWorkbench.vue'
import { channelLabel } from '../../core/chat/sessionList'
import ChatBanner from './ChatBanner.vue'
import ChatComposer from './ChatComposer.vue'
import ChatHeader from './ChatHeader.vue'
import ChatMessages from './ChatMessages.vue'
import ChatSessionCard from './ChatSessionCard.vue'
import { useChatBanner } from './useChatBanner'
import { useChatSession } from './useChatSession'

const view = useChatSession()
const {
  session,
  chat,
  sessionsOpen,
  saving,
  sendingImage,
  recalling,
  recallTarget,
  focusMessageId,
  privacyOpen,
  privacyDraft,
  init,
  openSession,
  startNewSession,
  send,
  retrySend,
  saveConversation,
  retryL1,
  attachImage,
  requestRecall,
  confirmRecall,
  openPrivacy,
  confirmPrivacy,
  backToMemory,
} = view

const { banner, degradedAttr, runAction } = useChatBanner({
  openPrivacy,
  retryL1,
  startNewSession,
})

const draft = ref('')

const viewState = computed(() => {
  if (session.loadingDetail && chat.messages.length === 0) {
    return 'loading'
  }
  if (session.error !== '' && chat.messages.length === 0) {
    return 'error'
  }
  if (chat.messages.length === 0 && !chat.streaming) {
    return 'empty'
  }
  return 'default'
})

const restricted = computed(() => {
  if (session.readonly) {
    return 'readonly'
  }
  if (chat.streaming || chat.sending || saving.value) {
    return 'locked'
  }
  return undefined
})

const busy = computed(() => session.loadingDetail || chat.streaming)
const composerDisabled = computed(() => session.error !== '' && chat.messages.length === 0)

const readonlyText = computed(() => {
  if (session.imported) {
    return '导入消息 · 只读'
  }
  const label = channelLabel(session.activeChannel) || '对应平台'
  return `只读会话 · 在 ${label} 中回复`
})

/** 点击卡片外部或按 Esc 收起会话卡片 */
function onDocumentPointerDown(event: PointerEvent): void {
  const target = event.target
  if (!(target instanceof Element)) {
    return
  }
  if (target.closest('#chat-sessions') || target.closest('.chat-head')) {
    return
  }
  sessionsOpen.value = false
}

function onDocumentKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape' && sessionsOpen.value) {
    sessionsOpen.value = false
  }
}

watch(sessionsOpen, (open) => {
  if (open) {
    document.addEventListener('pointerdown', onDocumentPointerDown)
    document.addEventListener('keydown', onDocumentKeydown)
    return
  }
  document.removeEventListener('pointerdown', onDocumentPointerDown)
  document.removeEventListener('keydown', onDocumentKeydown)
})

async function reloadActive(): Promise<void> {
  if (session.activeId) {
    await openSession(session.activeId, false)
  }
}

onMounted(() => {
  void init()
})

onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', onDocumentPointerDown)
  document.removeEventListener('keydown', onDocumentKeydown)
})
</script>

<template>
  <BaseWorkbench>
    <div
      class="chat"
      :class="{ 'is-sessions': sessionsOpen }"
      :data-view-state="viewState"
      :data-degraded="degradedAttr"
      :data-restricted="restricted"
      :aria-busy="busy ? 'true' : undefined"
    >
      <ChatHeader
        :sessions-open="sessionsOpen"
        :saving="saving"
        :back-to-memory="chat.cameFromMemory"
        @toggle-sessions="sessionsOpen = !sessionsOpen"
        @save="saveConversation"
        @back-to-memory="backToMemory"
      />

      <ChatBanner
        v-if="banner"
        :kind="banner.kind"
        :title="banner.title"
        :description="banner.description"
        :action="banner.action"
        :percent="banner.percent"
        @action="runAction"
      />

      <ChatMessages
        :focus-message-id="focusMessageId"
        @recall="requestRecall"
        @retry="retrySend"
        @reload="reloadActive"
      />

      <ChatComposer
        v-if="!session.readonly"
        v-model="draft"
        :disabled="composerDisabled"
        :locked="chat.streaming || chat.sending"
        :attaching="sendingImage"
        @send="send"
        @attach="attachImage"
      />

      <div v-else class="readonly-bar">
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <rect x="4" y="11" width="16" height="10" rx="2" />
          <path d="M8 11V7a4 4 0 0 1 8 0v4" />
        </svg>
        <span>{{ readonlyText }}</span>
      </div>

      <ChatSessionCard
        v-if="sessionsOpen"
        @select="openSession"
        @create="startNewSession"
        @close="sessionsOpen = false"
        @retry="session.loadList()"
      />
    </div>

    <BaseModal
      :open="recallTarget !== null"
      title="撤回这条消息"
      @close="recallTarget = null"
    >
      <p class="modal-text">撤回后消息内容在界面中折叠为系统提示，且无法恢复。</p>
      <template #foot>
        <BaseButton variant="ghost" @click="recallTarget = null">取消</BaseButton>
        <span class="spacer" />
        <BaseButton variant="primary" :loading="recalling" @click="confirmRecall">撤回</BaseButton>
      </template>
    </BaseModal>

    <BaseModal :open="privacyOpen" title="线上后端隐私确认" @close="privacyOpen = false">
      <p class="modal-text">
        对话内容（含记忆上下文）将发送至云端模型服务处理；开启图片理解时，聊天记录中的图片也会随理解请求发送。
      </p>
      <p class="modal-text">敏感内容建议改用本地部署；确认后仍可在设置中查看范围与时间。</p>
      <div class="modal-checks">
        <BaseCheckbox v-model="privacyDraft.accepted" label="我已知晓上述数据范围与去向" />
        <BaseCheckbox v-model="privacyDraft.persistent" label="下次不再提醒" />
      </div>
      <template #foot>
        <BaseButton variant="ghost" @click="privacyOpen = false">取消</BaseButton>
        <span class="spacer" />
        <BaseButton variant="primary" :disabled="!privacyDraft.accepted" @click="confirmPrivacy">
          确认
        </BaseButton>
      </template>
    </BaseModal>
  </BaseWorkbench>
</template>

<style scoped>
.chat {
  --chat-shift: 308px;

  position: relative;
  display: flex;
  width: 100%;
  max-width: 800px;
  height: 100%;
  max-height: 600px;
  min-height: 0;
  margin: 0 auto;
  flex-direction: column;
  gap: var(--space-14);
}

.chat .messages,
.chat :deep(.banner) {
  transition: margin-left var(--dur-base) var(--ease-out);
}

.chat.is-sessions .messages,
.chat.is-sessions :deep(.banner) {
  margin-left: var(--chat-shift);
}

.readonly-bar {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--space-8);
  min-height: var(--readonly-bar-h);
  padding: var(--space-10) var(--space-16);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  background: var(--bg-subtle);
  box-shadow: var(--shadow-sm);
  color: var(--text-2);
  font-size: var(--fs-small);
}

.readonly-bar svg {
  flex: none;
  width: 15px;
  height: 15px;
  fill: none;
  stroke: var(--text-3);
  stroke-width: 1.9;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.modal-text {
  margin: 0 0 var(--space-8);
  color: var(--text-2);
  font-size: var(--fs-body);
  line-height: var(--lh-body);
}

.modal-checks {
  display: flex;
  flex-direction: column;
  margin-top: var(--space-8);
}

.spacer {
  flex: 1;
}

@media (max-width: 900px) {
  .chat.is-sessions .messages,
  .chat.is-sessions :deep(.banner) {
    margin-left: 0;
  }
}
</style>
