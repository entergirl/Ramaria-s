/**
 * src/core/theme.ts — 主题（浅色 / 深色）
 *
 * 职责:
 * - 解析初始主题、应用到文档根节点、持久化用户选择
 *
 * 设计特点:
 * - 主题只经 `data-theme` 属性表达，配色由 tokens.css 的变量覆盖承载
 * - 初始化在首帧前同步完成并打 `data-theme-ready`，配合样式抑制首帧过渡
 * - 不跟随系统：只有浅色与深色两态，默认浅色
 */

import { PREF_KEYS, readPref, writePref } from './prefs'

export type ThemeName = 'light' | 'dark'

const DEFAULT_THEME: ThemeName = 'light'

/** 读取已保存主题；无记录或值非法时回退浅色 */
export function resolveStoredTheme(): ThemeName {
  return readPref(PREF_KEYS.theme) === 'dark' ? 'dark' : DEFAULT_THEME
}

/** 应用主题并持久化 */
export function applyTheme(theme: ThemeName): void {
  const root = document.documentElement
  root.dataset.theme = theme
  root.dataset.themeReady = 'true'
  writePref(PREF_KEYS.theme, theme)
}

/** 当前生效主题 */
export function currentTheme(): ThemeName {
  return document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light'
}

/** 在浅色与深色之间切换，返回切换后的主题 */
export function toggleTheme(): ThemeName {
  const next: ThemeName = currentTheme() === 'dark' ? 'light' : 'dark'
  applyTheme(next)
  return next
}

/** 启动时应用已保存主题（不写回存储） */
export function initTheme(): ThemeName {
  const theme = resolveStoredTheme()
  const root = document.documentElement
  root.dataset.theme = theme
  root.dataset.themeReady = 'true'
  return theme
}
