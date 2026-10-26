/**
 * src/stores/nav.ts — 导航位置偏好
 *
 * 职责:
 * - 维护圆键导航的位置预设，供圆键与设置页共用
 *
 * 设计特点:
 * - 预设只存本地偏好，不进后端配置
 * - 圆键与设置页读同一份状态：设置页改完，圆键立即回正
 * - 自由摆放坐标仍由圆键自己管理，切换预设时一并清除
 */

import { defineStore } from 'pinia'
import { ref } from 'vue'

import { NAV_PRESET_LABELS, type NavPreset } from '../components/nav/geometry'
import { PREF_KEYS, readPref, writePref } from '../core/prefs'

/** 读取已保存的预设；非法或缺失回到底中 */
function readPreset(): NavPreset {
  const saved = readPref(PREF_KEYS.navPosition)
  const presets = Object.keys(NAV_PRESET_LABELS) as NavPreset[]
  return presets.find((item) => item === saved) ?? 'bottom'
}

export const useNavStore = defineStore('nav', () => {
  const preset = ref<NavPreset>(readPreset())

  function setPreset(value: NavPreset): void {
    preset.value = value
    writePref(PREF_KEYS.navPosition, value)
  }

  return {
    preset,
    setPreset,
  }
})
