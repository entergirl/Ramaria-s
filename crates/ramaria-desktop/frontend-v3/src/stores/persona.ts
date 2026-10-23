/**
 * src/stores/persona.ts — 人格页的界面态
 *
 * 职责:
 * - 记录列表 / 详情二级切换、当前人格与一次性预选标志
 *
 * 设计特点:
 * - 列表与详情数据由视图装载，本 store 只存选择与一次性标志
 * - `preselectUid` 供跨视图跳转（记忆页 / 导入完成）预选后一次性消费
 */

import { defineStore } from 'pinia'
import { ref } from 'vue'

/** 人格页二级形态 */
export type PersonaPane = 'list' | 'detail'

export const usePersonaStore = defineStore('persona', () => {
  const pane = ref<PersonaPane>('list')
  const activeUid = ref<string | null>(null)
  const preselectUid = ref<string | null>(null)

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

  return {
    pane,
    activeUid,
    preselectUid,
    openDetail,
    backToList,
    requestPreselect,
    consumePreselect,
  }
})
