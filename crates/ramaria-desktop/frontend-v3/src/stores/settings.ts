/**
 * src/stores/settings.ts — 设置页的界面态
 *
 * 职责:
 * - 记录当前范围与子页，并记住每个范围最后一次停留的子页
 *
 * 设计特点:
 * - 只存导航界面态；配置本体、脏值与保存进度由视图组合式函数持有
 * - 子页 id 与深链段一致，切范围时回到该范围上次的子页
 * - 调试未解锁时「进阶设置」不渲染，本 store 不承担门控判断
 */

import { defineStore } from 'pinia'
import { ref } from 'vue'

import { type SettingsScope, defaultSectionId } from '../config/settingsSections'

export const useSettingsStore = defineStore('settings', () => {
  const scope = ref<SettingsScope>('basic')
  const sectionId = ref(defaultSectionId('basic'))
  const lastSection = ref<Record<SettingsScope, string>>({
    basic: defaultSectionId('basic'),
    advanced: defaultSectionId('advanced'),
  })

  /** 切到指定范围与子页，并记住该范围的选择 */
  function goTo(nextScope: SettingsScope, nextSection: string): void {
    scope.value = nextScope
    sectionId.value = nextSection
    lastSection.value = { ...lastSection.value, [nextScope]: nextSection }
  }

  /** 只切范围：落到该范围上次停留的子页 */
  function switchScope(nextScope: SettingsScope): void {
    goTo(nextScope, lastSection.value[nextScope])
  }

  return {
    scope,
    sectionId,
    lastSection,
    goTo,
    switchScope,
  }
})
