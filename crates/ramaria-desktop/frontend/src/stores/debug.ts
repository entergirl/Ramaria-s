/**
 * src/stores/debug.ts — 调试解锁与评估目录
 *
 * 职责:
 * - 维护解锁开关（持久化到本地偏好）与评估输出目录
 *
 * 设计特点:
 * - 解锁是门控不是状态：关闭时调试入口与进阶设置直接不渲染
 * - 解锁开关只存本地偏好，不进后端配置
 * - 评估目录仅存内存，重启后由用户重新选择
 */

import { defineStore } from 'pinia'
import { ref } from 'vue'

import { PREF_KEYS, readPref, writePref } from '../core/prefs'

export const useDebugStore = defineStore('debug', () => {
  const unlocked = ref(readPref(PREF_KEYS.debug) === '1')
  const evalDir = ref('')

  function setUnlocked(value: boolean): void {
    unlocked.value = value
    writePref(PREF_KEYS.debug, value ? '1' : '0')
  }

  function toggleUnlocked(): boolean {
    setUnlocked(!unlocked.value)
    return unlocked.value
  }

  return {
    unlocked,
    evalDir,
    setUnlocked,
    toggleUnlocked,
  }
})
