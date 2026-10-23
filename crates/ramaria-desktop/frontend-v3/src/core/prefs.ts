/**
 * src/core/prefs.ts — 本地偏好读写
 *
 * 职责:
 * - 统一 localStorage 键名与读写，读取失败时回退默认值
 *
 * 设计特点:
 * - 只存界面偏好（主题 / 导航位置 / 调试开关），不存业务数据与凭据
 * - 读写全部包裹容错：不可写环境下按会话内默认继续，不阻断渲染
 */

export const PREF_KEYS = {
  theme: 'ramaria.theme',
  navPosition: 'ramaria.navPosition',
  navFreeX: 'ramaria.navFreeX',
  navFreeY: 'ramaria.navFreeY',
  debug: 'ramaria.debug',
} as const

/** 读取偏好；存储不可用时返回 `null` */
export function readPref(key: string): string | null {
  try {
    return window.localStorage.getItem(key)
  } catch {
    return null
  }
}

/** 写入偏好；存储不可用时静默忽略 */
export function writePref(key: string, value: string): void {
  try {
    window.localStorage.setItem(key, value)
  } catch {
    // 隐私模式或配额耗尽：偏好不持久化，不影响当前会话
  }
}

/** 删除偏好；存储不可用时静默忽略 */
export function removePref(key: string): void {
  try {
    window.localStorage.removeItem(key)
  } catch {
    // 同上
  }
}
