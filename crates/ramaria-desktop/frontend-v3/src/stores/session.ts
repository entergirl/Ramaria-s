/**
 * src/stores/session.ts — 会话列表、当前会话与人格摘要
 *
 * 职责:
 * - 维护会话摘要列表、当前打开会话的详情与已读状态
 * - 缓存人格摘要，供会话卡片标签与对话头部取名称、来源
 * - 派生只读判据（社交通道或导入历史）
 *
 * 设计特点:
 * - 打开会话即标记已读，未读计数就地清零，不重取列表
 * - 主动消息只做未读自增，不重建列表
 * - 打开失败保留上一次详情，避免界面闪空
 * - 新建会话直接以摘要构造详情，少一次往返
 */

import { defineStore } from 'pinia'
import { computed, ref } from 'vue'

import { personaApi, sessionApi } from '../core/api'
import type { PersonaFull, SessionDetail, SessionSummary } from '../core/api/dto'
import { errorMessage } from '../core/errors'

export const useSessionStore = defineStore('session', () => {
  const items = ref<SessionSummary[]>([])
  const activeId = ref<string | null>(null)
  const active = ref<SessionDetail | null>(null)
  const loadingList = ref(false)
  const loadingDetail = ref(false)
  const error = ref('')

  const personas = ref<PersonaFull[]>([])
  const personasLoaded = ref(false)
  const personasError = ref('')

  const personaMap = computed(() => {
    const map = new Map<string, PersonaFull>()
    for (const persona of personas.value) {
      map.set(persona.uid, persona)
    }
    return map
  })

  /** 当前会话来源通道（无会话时按本地） */
  const activeChannel = computed(() => active.value?.channel ?? 'local')

  /** 会话归属人格；未绑定时回退产品人格（kind = rama） */
  const activePersona = computed<PersonaFull | null>(() => {
    const bound = active.value?.persona_uid
    if (bound && personaMap.value.has(bound)) {
      return personaMap.value.get(bound) ?? null
    }
    return defaultPersona.value
  })

  /** 产品自身人格（未指定人格时的对话对象） */
  const defaultPersona = computed<PersonaFull | null>(() => {
    return (
      personas.value.find((persona) => persona.kind === 'rama' && persona.is_active) ??
      personas.value.find((persona) => persona.kind === 'rama') ??
      null
    )
  })

  /** 用户本人人格（对话右侧「我」的头像与昵称来源） */
  const userPersona = computed<PersonaFull | null>(() => {
    return (
      personas.value.find((persona) => persona.kind === 'user' && persona.is_active) ??
      personas.value.find((persona) => persona.kind === 'user') ??
      null
    )
  })

  /** 导入历史会话：归属人格来源为 QQ */
  const imported = computed(() => activePersona.value?.source === 'qq')

  /** 只读：社交通道会话或导入历史 */
  const readonly = computed(() => activeChannel.value !== 'local' || imported.value)

  /** 当前会话已结束（历史会话，可浏览；继续输入会开始新会话） */
  const closed = computed(() => active.value !== null && active.value.ended_at !== null)

  function personaOf(uid: string | null | undefined): PersonaFull | null {
    if (!uid) {
      return null
    }
    return personaMap.value.get(uid) ?? null
  }

  async function loadPersonas(force = false): Promise<void> {
    if (personasLoaded.value && !force) {
      return
    }
    personasError.value = ''
    try {
      personas.value = await personaApi.listFull()
      personasLoaded.value = true
    } catch (failure) {
      personasError.value = errorMessage(failure, '人格列表加载失败')
    }
  }

  /** 局部替换人格缓存（改名 / 换头像后免整表重取；未加载时留待下次装载） */
  function patchPersona(updated: PersonaFull): void {
    const index = personas.value.findIndex((item) => item.uid === updated.uid)
    if (index >= 0) {
      personas.value[index] = updated
    }
  }

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
      applySummary(summary)
      return summary
    } catch (failure) {
      error.value = errorMessage(failure, '新建会话失败')
      return null
    }
  }

  /** 以摘要构造当前会话（新建会话后避免再取一次详情） */
  function applySummary(summary: SessionSummary): void {
    active.value = {
      id: summary.id,
      started_at: summary.started_at,
      ended_at: summary.ended_at,
      persona_uid: summary.persona_uid,
      channel: summary.channel,
      total_messages: summary.message_count,
      has_more: false,
      messages: [],
    }
    activeId.value = summary.id
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
    personas,
    personasLoaded,
    personasError,
    activeChannel,
    activePersona,
    defaultPersona,
    userPersona,
    imported,
    readonly,
    closed,
    personaOf,
    loadPersonas,
    patchPersona,
    loadList,
    open,
    create,
    applySummary,
    markRead,
    bumpUnread,
    clearActive,
  }
})
