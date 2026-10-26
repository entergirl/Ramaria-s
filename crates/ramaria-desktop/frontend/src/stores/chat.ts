/**
 * src/stores/chat.ts — 消息列表、流式与隐私门禁
 *
 * 职责:
 * - 维护当前会话的消息序列、流式缓冲与发送门禁
 * - 消费 `chat-delta` / `chat-done` / `chat-error` 与 `proactive-message`
 * - 读取隐私确认状态，未确认时锁定发送
 * - 生成结束后按服务端最新页对账，替换乐观消息与流式占位
 *
 * 设计特点:
 * - 流式以 request_id 甄别，非本次请求的增量与错误直接忽略
 * - 完成事件以全文为准覆盖累积缓冲，避免增量丢失导致的截断
 * - 对账只在消息所属会话仍是当前会话时执行，切走后不污染新列表
 * - 主动消息按 message_id 去重；非当前会话只做未读自增
 * - 到达即已读：回复与主动消息落到当前会话时推进已读时间，保证托盘徽标与界面一致
 */
import { defineStore } from 'pinia'
import { computed, ref } from 'vue'

import { chatApi, sessionApi } from '../core/api'
import type {
  ChatDeltaPayload,
  ChatDonePayload,
  ChatErrorPayload,
  PrivacyStatus,
  ProactiveMessagePayload,
  SendLocalImageResult,
  SessionMessage,
} from '../core/api/dto'
import { appendMessage, mergeServerPage } from '../core/chat/merge'
import { streamDisplay } from '../core/chat/bubble'
import { errorMessage } from '../core/errors'
import { EVENTS, useEvent } from '../core/events'
import { useSessionStore } from './session'

/** 生成结束后回读的消息条数（只取尾部，避免大库全量重复加载） */
const RECONCILE_LIMIT = 60

export const useChatStore = defineStore('chat', () => {
  const messages = ref<SessionMessage[]>([])
  const sending = ref(false)
  const requestId = ref<string | null>(null)
  /** 本次请求所属会话（用于对账与切换视图时的归属判断） */
  const requestSessionId = ref<string | null>(null)
  const streamingText = ref('')
  const error = ref('')
  /** 本次请求失败时的用户原文，供重试入口复用 */
  const failedText = ref('')
  const privacy = ref<PrivacyStatus | null>(null)
  const privacySaving = ref(false)
  /** 通知点击定位：目标会话与消息 */
  const activation = ref<{ sessionId: string; messageId: string } | null>(null)
  /** L1 未生成的会话（对话横幅提示与重试入口） */
  const l1IssueSessionId = ref<string | null>(null)
  /** 本次进入来自记忆视图（「返回记忆」入口的一次性标志） */
  const cameFromMemory = ref(false)

  /** 是否正在流式接收 */
  const streaming = computed(() => requestId.value !== null)

  /** 流式展示文本：分隔符临时显示为换行 */
  const streamingDisplay = computed(() => streamDisplay(streamingText.value))

  /** 输入区是否可用：非发送中、非流式、隐私已确认 */
  const canCompose = computed(
    () =>
      !sending.value && !streaming.value && privacy.value?.status !== 'NeedsConfirmation',
  )

  /** 隐私是否待确认（用于降级条与发送锁定） */
  const privacyNeedsConfirm = computed(() => privacy.value?.status === 'NeedsConfirmation')

  /** 最近一条用户消息（用于失败重试） */
  function lastUserText(): string {
    for (let index = messages.value.length - 1; index >= 0; index -= 1) {
      const message = messages.value[index]
      if (message.role === 'user') {
        return message.content
      }
    }
    return ''
  }

  /** 装载会话消息（切换会话时清空流式与错误态） */
  function loadMessages(list: SessionMessage[]): void {
    messages.value = [...list]
    streamingText.value = ''
    requestId.value = null
    requestSessionId.value = null
    error.value = ''
    failedText.value = ''
  }

  /**
   * 发送一条文本消息。
   *
   * 参数:
   * - `text`: 用户输入（已由调用方 trim）
   * - `personaUid`: 目标人格（缺省人格由服务层补全）
   * - `sessionId`: 目标会话（调用方需先确保会话存在）
   *
   * 返回:
   * - 是否已进入流式接收
   */
  async function send(text: string, personaUid: string | null, sessionId: string): Promise<boolean> {
    const trimmed = text.trim()
    if (!trimmed || !canCompose.value) {
      return false
    }

    const optimistic: SessionMessage = {
      id: `local-${Date.now()}`,
      role: 'user',
      content: trimmed,
      persona_uid: personaUid,
      created_at: Date.now(),
      is_proactive: false,
      sender_ref: null,
      sender_name: null,
      recalled_at: null,
    }
    messages.value = [...messages.value, optimistic]
    sending.value = true
    error.value = ''
    failedText.value = ''
    try {
      requestSessionId.value = sessionId
      requestId.value = await chatApi.send(trimmed, personaUid, sessionId)
      streamingText.value = ''
      return true
    } catch (failure) {
      // 发送前置校验失败时消息未落库，移除乐观气泡，避免留下幽灵消息
      messages.value = messages.value.filter((message) => message.id !== optimistic.id)
      error.value = errorMessage(failure, '发送失败')
      failedText.value = trimmed
      requestId.value = null
      requestSessionId.value = null
      return false
    } finally {
      sending.value = false
    }
  }

  /** 追加一张已落库的本地图片消息 */
  function appendImage(message: SendLocalImageResult): void {
    messages.value = appendMessage(messages.value, {
      id: message.messageId,
      role: 'user',
      content: message.content,
      persona_uid: null,
      created_at: Date.now(),
      is_proactive: false,
      sender_ref: null,
      sender_name: null,
      recalled_at: null,
    })
  }

  /**
   * 与提交后的用户消息对齐（图片发送后调用）。
   *
   * 说明:
   * - 图片命令已返回真实消息标识，但仍需以服务端列表为准补齐时间与归属
   */
  async function reconcile(sessionId: string): Promise<void> {
    // 新一轮生成已开始时不对账：避免把刚插入的乐观消息覆盖掉
    if (requestId.value !== null) {
      return
    }
    try {
      const page = await sessionApi.get(sessionId, RECONCILE_LIMIT)
      const session = useSessionStore()
      if (session.activeId !== sessionId || requestId.value !== null) {
        return
      }
      messages.value = mergeServerPage(messages.value, page.messages)
    } catch {
      // 对账失败保留本地列表，不打断阅读
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

    const sessionId = requestSessionId.value
    requestSessionId.value = null
    if (sessionId) {
      void reconcile(sessionId)
      const session = useSessionStore()
      if (session.activeId === sessionId) {
        // 回复落到当前会话即视为已读：推进已读时间以同步托盘徽标
        void session.markRead(sessionId)
      } else {
        session.bumpUnread(sessionId)
        void session.loadList()
      }
    }
  }

  function handleError(payload: ChatErrorPayload): void {
    if (payload.request_id !== requestId.value) {
      return
    }
    error.value = payload.error_detail || payload.error_title
    failedText.value = lastUserText()
    streamingText.value = ''
    requestId.value = null
    requestSessionId.value = null
  }

  /** 重试上一次失败的用户输入；返回原文供调用方重新发送 */
  function takeFailedText(): string {
    const text = failedText.value
    failedText.value = ''
    error.value = ''
    return text
  }

  /** 收回一条本地消息；返回失败原因（成功为 null） */
  async function recall(messageId: string): Promise<string | null> {
    try {
      await chatApi.recallMessage(messageId)
      const hit = messages.value.find((message) => message.id === messageId)
      if (hit) {
        hit.recalled_at = Date.now()
      }
      return null
    } catch (failure) {
      return errorMessage(failure, '撤回失败')
    }
  }

  async function loadPrivacy(): Promise<void> {
    try {
      privacy.value = await chatApi.checkPrivacy()
    } catch {
      // 隐私状态读取失败不阻塞浏览，发送时由服务层再次校验
    }
  }

  /** 确认隐私范围；返回失败原因（成功为 null） */
  async function confirmPrivacy(persistent: boolean): Promise<string | null> {
    privacySaving.value = true
    try {
      await chatApi.confirmPrivacy(persistent)
      privacy.value = { status: 'Confirmed', persistent, confirmed_at: Date.now() }
      return null
    } catch (failure) {
      return errorMessage(failure, '隐私确认失败')
    } finally {
      privacySaving.value = false
    }
  }

  /** 主动消息：当前会话追加；其它会话未读自增 */
  function applyProactive(payload: ProactiveMessagePayload): void {
    const session = useSessionStore()
    if (payload.activated) {
      activation.value = { sessionId: payload.session_id, messageId: payload.message_id }
      return
    }
    if (session.activeId === payload.session_id) {
      messages.value = appendMessage(messages.value, {
        id: payload.message_id,
        role: 'assistant',
        content: payload.content,
        persona_uid: payload.persona || null,
        created_at: payload.created_at || Date.now(),
        is_proactive: true,
        sender_ref: null,
        sender_name: null,
        recalled_at: null,
      })
      // 消息落到当前会话即视为已读：推进已读时间以同步托盘徽标
      void session.markRead(payload.session_id)
      return
    }

    const known = session.items.some((item) => item.id === payload.session_id)
    session.bumpUnread(payload.session_id)
    if (!known) {
      void session.loadList()
    }
  }

  /** 消费一次通知定位（返回后清空） */
  function takeActivation(): { sessionId: string; messageId: string } | null {
    const pending = activation.value
    activation.value = null
    return pending
  }

  /** 标记 L1 未生成（保存对话后置位） */
  function markL1Issue(sessionId: string | null): void {
    l1IssueSessionId.value = sessionId
  }

  /** 记录本次进入来自记忆视图（供「返回记忆」入口渲染） */
  function markFromMemory(): void {
    cameFromMemory.value = true
  }

  /** 清空来源标记（离开对话视图时调用） */
  function clearOrigin(): void {
    cameFromMemory.value = false
  }

  function clear(): void {
    messages.value = []
    streamingText.value = ''
    requestId.value = null
    requestSessionId.value = null
    error.value = ''
    failedText.value = ''
  }

  useEvent(EVENTS.chatDelta, handleDelta)
  useEvent(EVENTS.chatDone, handleDone)
  useEvent(EVENTS.chatError, handleError)
  useEvent(EVENTS.proactiveMessage, applyProactive)

  return {
    messages,
    sending,
    requestId,
    requestSessionId,
    streamingText,
    streamingDisplay,
    error,
    failedText,
    privacy,
    privacySaving,
    activation,
    l1IssueSessionId,
    cameFromMemory,
    streaming,
    canCompose,
    privacyNeedsConfirm,
    loadMessages,
    send,
    appendImage,
    reconcile,
    takeFailedText,
    recall,
    loadPrivacy,
    confirmPrivacy,
    applyProactive,
    takeActivation,
    markL1Issue,
    markFromMemory,
    clearOrigin,
    clear,
  }
})
