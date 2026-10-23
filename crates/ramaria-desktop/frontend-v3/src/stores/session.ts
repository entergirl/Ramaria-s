/**
 * src/stores/session.ts — 会话列表与当前会话
 *
 * 职责:
 * - 维护会话摘要列表、当前打开会话的详情与已读状态
 * - 派生只读判据（来源通道非本地即只读）
 *
 * 设计特点:
 * - 打开会话即标记已读，未读计数就地清零，不重取列表
 * - 主动消息只做未读自增，不重建列表
 * - 打开失败保留上一次详情，避免界面闪空
 */

import { defineStore } from 'pinia'
import { computed, ref } from 'vue'

import { sessionApi } from '../core/api'
import type { SessionDetail, SessionSummary } from '../core/api/dto'
import { errorMessage } from '../core/errors'

export const useSessionStore = defineStore('session', () => {
  const items = ref<SessionSummary[]>([])
  const activeId = ref<string | null>(null)
  const active = ref<SessionDetail | null>(null)
  const loadingList = ref(false)
  const loadingDetail = ref(false)
  const error = ref('')

  /** 当前会话来源通道（无会话时按本地） */
  const activeChannel = computed(() => active.value?.channel ?? 'local')

  /** 社交会话只读：隐藏输入区并显示只读横幅 */
  const readonly = computed(() => activeChannel.value !== 'local')

  async function loadList(): Promise<void> {
    loadingList.value = true
    error.value = ''
    try {
      items.value = await sessionApi.list()
    } catch (failure) {
      error.value = errorMessage(failure, '会话列表加载失败')
    } finally {
      loadingList.value = false
    }
  }

  async function open(sessionId: string): Promise<SessionDetail | null> {
    loadingDetail.value = true
    error.value = ''
    try {
      const detail = await sessionApi.get(sessionId)
      active.value = detail
      activeId.value = detail.id
      await markRead(detail.id)
      return detail
    } catch (failure) {
      error.value = errorMessage(failure, '会话加载失败')
      return null
    } finally {
      loadingDetail.value = false
    }
  }

  async function create(personaUid: string | null = null): Promise<SessionSummary | null> {
    try {
      const summary = await sessionApi.create(personaUid)
      items.value = [summary, ...items.value]
      return summary
    } catch (failure) {
      error.value = errorMessage(failure, '新建会话失败')
      return null
    }
  }

  async function markRead(sessionId: string): Promise<void> {
    try {
      await sessionApi.markRead(sessionId)
      const hit = items.value.find((item) => item.id === sessionId)
      if (hit) {
        hit.unread = 0
      }
    } catch {
      // 已读落库失败不影响浏览，下次打开会重试
    }
  }

  /** 主动消息到达：非当前会话未读自增（当前会话由 chat store 直接追加，不计数） */
  function bumpUnread(sessionId: string): void {
    if (activeId.value === sessionId) {
      return
    }
    const hit = items.value.find((item) => item.id === sessionId)
    if (hit) {
      hit.unread += 1
    }
  }

  function clearActive(): void {
    active.value = null
    activeId.value = null
  }

  return {
    items,
    activeId,
    active,
    loadingList,
    loadingDetail,
    error,
    activeChannel,
    readonly,
    loadList,
    open,
    create,
    markRead,
    bumpUnread,
    clearActive,
  }
})
