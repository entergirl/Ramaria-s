/**
 * src/stores/chat.ts — 消息列表、流式与隐私门禁
 *
 * 职责:
 * - 维护当前会话的消息序列、流式缓冲与发送门禁
 * - 消费 `chat-delta` / `chat-done` / `chat-error` 三个事件
 * - 读取隐私确认状态，未确认时锁定发送
 *
 * 设计特点:
 * - 流式以 request_id 甄别，非本次请求的增量与错误直接忽略
 * - 完成事件以全文为准覆盖累积缓冲，避免增量丢失导致的截断
 * - 事件订阅在 store 作用域内注册，随作用域销毁取消
 */

import { defineStore } from 'pinia'
import { computed, ref } from 'vue'

import { chatApi } from '../core/api'
import type {
  ChatDeltaPayload,
  ChatDonePayload,
  ChatErrorPayload,
  PrivacyStatus,
  SessionMessage,
} from '../core/api/dto'
import { errorMessage } from '../core/errors'
import { EVENTS, useEvent } from '../core/events'

export const useChatStore = defineStore('chat', () => {
  const messages = ref<SessionMessage[]>([])
  const sending = ref(false)
  const requestId = ref<string | null>(null)
  const streamingText = ref('')
  const error = ref('')
  const privacy = ref<PrivacyStatus | null>(null)
  const privacySaving = ref(false)

  /** 是否正在流式接收 */
  const streaming = computed(() => requestId.value !== null)

  /** 输入区是否可用：非发送中、非流式、隐私已确认 */
  const canCompose = computed(
    () =>
      !sending.value &&
      !streaming.value &&
      privacy.value?.status !== 'NeedsConfirmation',
  )

  /** 隐私是否待确认（用于降级条与发送锁定） */
  const privacyNeedsConfirm = computed(() => privacy.value?.status === 'NeedsConfirmation')

  function loadMessages(list: SessionMessage[]): void {
    messages.value = [...list]
    streamingText.value = ''
    requestId.value = null
    error.value = ''
  }

  async function send(
    text: string,
    personaUid: string | null,
    sessionId: string | null,
  ): Promise<boolean> {
    const trimmed = text.trim()
    if (!trimmed || !canCompose.value) {
      return false
    }
    sending.value = true
    error.value = ''
    try {
      requestId.value = await chatApi.send(trimmed, personaUid, sessionId)
      streamingText.value = ''
      return true
    } catch (failure) {
      error.value = errorMessage(failure, '发送失败')
      requestId.value = null
      return false
    } finally {
      sending.value = false
    }
  }

  function handleDelta(payload: ChatDeltaPayload): void {
    if (payload.request_id !== requestId.value) {
      return
    }
    streamingText.value += payload.content
  }

  function handleDone(payload: ChatDonePayload): void {
    if (payload.request_id !== requestId.value) {
      return
    }
    const finished: SessionMessage = {
      id: `pending-${payload.request_id}`,
      role: 'assistant',
      content: payload.content,
      persona_uid: null,
      created_at: Date.now(),
      is_proactive: false,
      sender_ref: null,
      sender_name: null,
      recalled_at: null,
    }
    messages.value = [...messages.value, finished]
    streamingText.value = ''
    requestId.value = null
  }

  function handleError(payload: ChatErrorPayload): void {
    if (payload.request_id !== requestId.value) {
      return
    }
    error.value = payload.error_detail || payload.error_title
    streamingText.value = ''
    requestId.value = null
  }

  async function loadPrivacy(): Promise<void> {
    try {
      privacy.value = await chatApi.checkPrivacy()
    } catch (failure) {
      error.value = errorMessage(failure, '隐私状态读取失败')
    }
  }

  async function confirmPrivacy(persistent: boolean): Promise<void> {
    privacySaving.value = true
    try {
      await chatApi.confirmPrivacy(persistent)
      privacy.value = { status: 'Confirmed', persistent, confirmed_at: Date.now() }
    } catch (failure) {
      error.value = errorMessage(failure, '隐私确认失败')
    } finally {
      privacySaving.value = false
    }
  }

  function clear(): void {
    messages.value = []
    streamingText.value = ''
    requestId.value = null
    error.value = ''
  }

  useEvent(EVENTS.chatDelta, handleDelta)
  useEvent(EVENTS.chatDone, handleDone)
  useEvent(EVENTS.chatError, handleError)

  return {
    messages,
    sending,
    requestId,
    streamingText,
    error,
    privacy,
    privacySaving,
    streaming,
    canCompose,
    privacyNeedsConfirm,
    loadMessages,
    send,
    loadPrivacy,
    confirmPrivacy,
    clear,
  }
})
