/**
 * src/stores/persona.ts — 人格页的界面态与数据缓存
 *
 * 职责:
 * - 记录列表 / 详情二级切换、当前人格与一次性预选标志
 * - 缓存人格全字段列表、默认对话人格标记与各人格的画像状态
 *
 * 设计特点:
 * - 列表数据留在本 store：返回列表不重新取数，保存后按 uid 局部替换
 * - 画像状态按人格 uid 缓存，读取失败记入错误表供卡内降级提示
 * - `preselectUid` 供跨视图跳转（记忆页 / 导入完成）预选后一次性消费
 */

import { defineStore } from 'pinia'
import { ref } from 'vue'

import type { PersonaFull, ProfileStatus } from '../core/api/dto'

/** 人格页二级形态 */
export type PersonaPane = 'list' | 'detail'

export const usePersonaStore = defineStore('persona', () => {
  const pane = ref<PersonaPane>('list')
  const activeUid = ref<string | null>(null)
  const preselectUid = ref<string | null>(null)

  const personas = ref<PersonaFull[]>([])
  const listLoading = ref(false)
  const listLoaded = ref(false)
  const listError = ref('')

  const defaultUid = ref<string | null>(null)
  const defaultLoaded = ref(false)
  const defaultError = ref('')

  const statusCache = ref<Record<string, ProfileStatus>>({})
  const statusLoading = ref<Record<string, boolean>>({})
  const statusErrors = ref<Record<string, string>>({})

  function openDetail(uid: string): void {
    activeUid.value = uid
    pane.value = 'detail'
  }

  function backToList(): void {
    pane.value = 'list'
    activeUid.value = null
  }

  function requestPreselect(uid: string | null): void {
    preselectUid.value = uid
  }

  function consumePreselect(): string | null {
    const uid = preselectUid.value
    preselectUid.value = null
    return uid
  }

  /** 按 uid 替换或追加一条人格（保存 / 导入后的局部刷新） */
  function upsert(persona: PersonaFull): void {
    const index = personas.value.findIndex((item) => item.uid === persona.uid)
    if (index >= 0) {
      const next = [...personas.value]
      next[index] = persona
      personas.value = next
      return
    }
    personas.value = [...personas.value, persona]
  }

  /** 该人格的画像状态读取状态：loading / error / ready / idle */
  function statusStateOf(uid: string): 'idle' | 'loading' | 'error' | 'ready' {
    if (statusLoading.value[uid]) {
      return 'loading'
    }
    if (statusErrors.value[uid]) {
      return 'error'
    }
    return statusCache.value[uid] ? 'ready' : 'idle'
  }

  /** 写入画像状态（成功清错，失败记错并丢缓存） */
  function setStatus(uid: string, status: ProfileStatus | null, error = ''): void {
    const nextCache = { ...statusCache.value }
    const nextErrors = { ...statusErrors.value }
    if (status) {
      nextCache[uid] = status
      delete nextErrors[uid]
    } else {
      delete nextCache[uid]
      if (error) {
        nextErrors[uid] = error
      } else {
        delete nextErrors[uid]
      }
    }
    statusCache.value = nextCache
    statusErrors.value = nextErrors
  }

  /** 丢弃画像状态缓存（重载画像后重新读取） */
  function clearStatus(uid: string): void {
    setStatus(uid, null)
  }

  return {
    pane,
    activeUid,
    preselectUid,
    personas,
    listLoading,
    listLoaded,
    listError,
    defaultUid,
    defaultLoaded,
    defaultError,
    statusCache,
    statusLoading,
    statusErrors,
    openDetail,
    backToList,
    requestPreselect,
    consumePreselect,
    upsert,
    statusStateOf,
    setStatus,
    clearStatus,
  }
})
