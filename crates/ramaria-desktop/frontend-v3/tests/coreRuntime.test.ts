/**
 * tests/coreRuntime.test.ts — 运行时纯逻辑
 *
 * 覆盖:
 * - 提示队列上限三条、错误类不自动消失、手动关闭
 * - 主题读取、应用、切换与持久化
 * - 错误归一保留命令名与兜底文案
 */

import { afterEach, describe, expect, it, vi } from 'vitest'

import { errorMessage, toAppError } from '../src/core/errors'
import { PREF_KEYS } from '../src/core/prefs'
import { applyTheme, currentTheme, initTheme, resolveStoredTheme, toggleTheme } from '../src/core/theme'
import { dismissToast, pushToast, toasts } from '../src/core/toast'
import { TransportError } from '../src/core/transport'

function clearToasts(): void {
  for (const item of toasts.value) {
    dismissToast(item.id)
  }
}

afterEach(() => {
  clearToasts()
  vi.useRealTimers()
  window.localStorage.clear()
  document.documentElement.removeAttribute('data-theme')
  document.documentElement.removeAttribute('data-theme-ready')
})

describe('提示队列', () => {
  it('同时最多三条，新提示顶掉最旧的一条', () => {
    pushToast({ message: '一' })
    pushToast({ message: '二' })
    pushToast({ message: '三' })
    pushToast({ message: '四' })
    expect(toasts.value.map((item) => item.message)).toEqual(['二', '三', '四'])
  })

  it('错误类不自动消失，其余四秒后自动收起', () => {
    vi.useFakeTimers()
    pushToast({ tone: 'danger', message: '构建失败' })
    pushToast({ tone: 'info', message: '已切换服务' })
    vi.advanceTimersByTime(4_000)
    expect(toasts.value.map((item) => item.message)).toEqual(['构建失败'])
  })

  it('关闭按键按标识移除', () => {
    const id = pushToast({ tone: 'danger', message: '可选操作', action: { label: '重试', run: () => {} } })
    expect(toasts.value[0]?.action?.label).toBe('重试')
    dismissToast(id)
    expect(toasts.value).toHaveLength(0)
  })
})

describe('主题', () => {
  it('无记录时回退浅色，存储值只在深色时生效', () => {
    expect(resolveStoredTheme()).toBe('light')
    window.localStorage.setItem(PREF_KEYS.theme, 'dark')
    expect(resolveStoredTheme()).toBe('dark')
    window.localStorage.setItem(PREF_KEYS.theme, 'weird')
    expect(resolveStoredTheme()).toBe('light')
  })

  it('应用主题写入根节点与本地存储，切换在两态间往返', () => {
    applyTheme('dark')
    expect(document.documentElement.dataset.theme).toBe('dark')
    expect(window.localStorage.getItem(PREF_KEYS.theme)).toBe('dark')
    expect(currentTheme()).toBe('dark')

    expect(toggleTheme()).toBe('light')
    expect(currentTheme()).toBe('light')
  })

  it('启动读取已保存主题但不回写', () => {
    window.localStorage.setItem(PREF_KEYS.theme, 'dark')
    expect(initTheme()).toBe('dark')
    expect(document.documentElement.dataset.theme).toBe('dark')
    expect(document.documentElement.dataset.themeReady).toBe('true')
  })
})

describe('错误归一', () => {
  it('传输层错误保留命令名并标记来源', () => {
    const error = toAppError(new TransportError('list_sessions', '桌面运行时不可用'))
    expect(error).toEqual({
      command: 'list_sessions',
      message: '桌面运行时不可用',
      transport: true,
    })
  })

  it('普通异常与原样字符串取自身文案，未知值用兜底', () => {
    expect(errorMessage(new Error('数据库被占用'))).toBe('数据库被占用')
    expect(errorMessage('图片序号越界')).toBe('图片序号越界')
    expect(errorMessage(null, '操作失败')).toBe('操作失败')
    expect(errorMessage(new Error(''), '操作失败')).toBe('操作失败')
  })
})
