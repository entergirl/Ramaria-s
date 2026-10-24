/**
 * src/stores/memory.ts — 记忆浏览的界面态与数据缓存
 *
 * 职责:
 * - 保存四页签的当前选择、人格筛选与切走前的快照
 * - 按人格缓存四层浏览数据（L1 / L2 / L3 / 知识），供页签切换复用
 *
 * 设计特点:
 * - 界面态与数据分离：页签 / 人格 / 滚动 / 筛选留在本 store，取数由视图按需触发
 * - 缓存以人格 UID 为键：切回同一人格先出旧数据再后台刷新，不跨人格串页
 * - 快照存内存不落 localStorage（D-V30-004 不用 KeepAlive）
 */

import { defineStore } from 'pinia'
import { computed, ref } from 'vue'

import type {
  FactList,
  MemoryEvent,
  MemoryL1,
  PersonaSummary,
  PersonalityProfile,
  ProfileStatus,
} from '../core/api/dto'

/** 记忆页签 */
export type MemoryTab = 'l1' | 'l2' | 'l3' | 'knowledge'

/** 切走前的恢复点 */
export interface MemorySnapshot {
  personaUid: string | null
  tab: MemoryTab
  scrollTop: number
  filter: string
}

/** 单个人格的四层浏览数据 */
export interface MemoryData {
  personaUid: string
  l1: MemoryL1[]
  l2: MemoryEvent[]
  profile: PersonalityProfile | null
  status: ProfileStatus | null
  facts: FactList | null
  /** 装载完成时刻（Unix 毫秒），供视图判断新鲜度 */
  loadedAt: number
}

/** 四页签条数（页签徽标与空态判据） */
export interface MemoryCounts {
  l1: number
  l2: number
  l3: number
  knowledge: number
}

export const useMemoryStore = defineStore('memory', () => {
  const tab = ref<MemoryTab>('l1')
  const personaUid = ref<string | null>(null)
  const filter = ref('')
  const snapshot = ref<MemorySnapshot | null>(null)

  const personas = ref<PersonaSummary[]>([])
  const personasLoading = ref(false)
  const personasError = ref('')

  const caches = ref<Record<string, MemoryData>>({})
  const loading = ref(false)
  const refreshing = ref(false)
  const error = ref('')

  /** 当前人格的缓存数据（切人格不混用） */
  const data = computed<MemoryData | null>(() => {
    const uid = personaUid.value
    if (!uid) {
      return null
    }
    return caches.value[uid] ?? null
  })

  /** 当前人格的页签条数 */
  const counts = computed<MemoryCounts>(() => {
    const current = data.value
    if (!current) {
      return { l1: 0, l2: 0, l3: 0, knowledge: 0 }
    }
    const knowledge = current.facts
      ? Object.values(current.facts.grouped).reduce((total, facts) => total + facts.length, 0)
      : 0
    return {
      l1: current.l1.length,
      l2: current.l2.length,
      l3:
        (current.profile?.base.length ?? 0) +
        (current.profile?.primary.length ?? 0) +
        (current.profile?.accent.length ?? 0),
      knowledge,
    }
  })

  /** 当前人格的显示名（工具栏与跳转参数） */
  const activePersona = computed<PersonaSummary | null>(() => {
    const uid = personaUid.value
    if (!uid) {
      return null
    }
    return personas.value.find((persona) => persona.uid === uid) ?? null
  })

  function selectPersona(uid: string | null): void {
    if (personaUid.value === uid) {
      return
    }
    personaUid.value = uid
    filter.value = ''
    snapshot.value = null
    error.value = ''
  }

  function setTab(next: MemoryTab): void {
    tab.value = next
  }

  function setData(payload: MemoryData): void {
    caches.value = { ...caches.value, [payload.personaUid]: payload }
  }

  function clearData(uid: string | null = personaUid.value): void {
    if (!uid) {
      return
    }
    const next = { ...caches.value }
    delete next[uid]
    caches.value = next
  }

  function saveSnapshot(scrollTop: number): void {
    snapshot.value = {
      personaUid: personaUid.value,
      tab: tab.value,
      scrollTop,
      filter: filter.value,
    }
  }

  function consumeSnapshot(): MemorySnapshot | null {
    const saved = snapshot.value
    snapshot.value = null
    return saved
  }

  return {
    tab,
    personaUid,
    filter,
    snapshot,
    personas,
    personasLoading,
    personasError,
    caches,
    loading,
    refreshing,
    error,
    data,
    counts,
    activePersona,
    selectPersona,
    setTab,
    setData,
    clearData,
    saveSnapshot,
    consumeSnapshot,
  }
})
