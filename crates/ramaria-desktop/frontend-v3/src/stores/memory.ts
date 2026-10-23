/**
 * src/stores/memory.ts — 记忆浏览的界面态
 *
 * 职责:
 * - 保存四页签的当前选择、人格筛选与切走前的快照
 *
 * 设计特点:
 * - 只存界面态（页签 / 人格 / 滚动 / 筛选），数据由视图按需装载
 * - 快照存内存不落 localStorage（D-V30-004 不用 KeepAlive）
 * - 切人格清空快照，避免跨人格串页
 */

import { defineStore } from 'pinia'
import { ref } from 'vue'

/** 记忆页签 */
export type MemoryTab = 'l1' | 'l2' | 'l3' | 'knowledge'

/** 切走前的恢复点 */
export interface MemorySnapshot {
  personaUid: string | null
  tab: MemoryTab
  scrollTop: number
  filter: string
}

export const useMemoryStore = defineStore('memory', () => {
  const tab = ref<MemoryTab>('l1')
  const personaUid = ref<string | null>(null)
  const filter = ref('')
  const snapshot = ref<MemorySnapshot | null>(null)

  function selectPersona(uid: string | null): void {
    if (personaUid.value === uid) {
      return
    }
    personaUid.value = uid
    tab.value = 'l1'
    filter.value = ''
    snapshot.value = null
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
    selectPersona,
    saveSnapshot,
    consumeSnapshot,
  }
})
