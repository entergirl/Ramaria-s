/**
 * src/stores/settings.ts — 设置页的界面态
 *
 * 职责:
 * - 记录基础 / 进阶范围、当前分区与未保存键集合
 *
 * 设计特点:
 * - 只存界面态；配置项本体与保存由视图经 configApi 处理
 * - 脏值按「设置键」记录，区块级保存成功后由视图清除
 * - 保存中状态供 `locked` 叠加态使用
 */

import { defineStore } from 'pinia'
import { computed, ref } from 'vue'

/** 设置范围 */
export type SettingsScope = 'basic' | 'advanced'

export const useSettingsStore = defineStore('settings', () => {
  const scope = ref<SettingsScope>('basic')
  const sectionId = ref('model')
  const dirtyKeys = ref<Set<string>>(new Set())
  const saving = ref(false)

  const dirty = computed(() => dirtyKeys.value.size > 0)

  function markDirty(key: string): void {
    dirtyKeys.value = new Set(dirtyKeys.value).add(key)
  }

  function clearDirty(keys: string[]): void {
    const next = new Set(dirtyKeys.value)
    for (const key of keys) {
      next.delete(key)
    }
    dirtyKeys.value = next
  }

  function clearAllDirty(): void {
    dirtyKeys.value = new Set()
  }

  function goTo(nextScope: SettingsScope, nextSection: string): void {
    scope.value = nextScope
    sectionId.value = nextSection
  }

  return {
    scope,
    sectionId,
    dirtyKeys,
    saving,
    dirty,
    markDirty,
    clearDirty,
    clearAllDirty,
    goTo,
  }
})
