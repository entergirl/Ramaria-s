/**
 * src/core/toast.ts — 轻提示队列
 *
 * 职责:
 * - 维护最多 3 条提示，按类型决定是否自动消失
 *
 * 设计特点:
 * - 队列为响应式数组，由 ToastLayer 渲染，调用方不直接操作 DOM
 * - 错误类不自动消失，需用户关闭，避免关键失败被错过
 * - 新提示顶掉最旧的一条，保证同时不超过 3 条
 */

import { readonly, ref } from 'vue'

export type ToastTone = 'info' | 'success' | 'warn' | 'danger'

export interface ToastAction {
  label: string
  run: () => void
}

export interface ToastItem {
  id: number
  tone: ToastTone
  message: string
  action?: ToastAction
}

const MAX_TOASTS = 3
const AUTO_DISMISS_MS = 4000

const items = ref<ToastItem[]>([])
let nextId = 1

/** 当前提示队列（只读） */
export const toasts = readonly(items)

/** 入队一条提示，返回其 id */
export function pushToast(input: {
  tone?: ToastTone
  message: string
  action?: ToastAction
}): number {
  const id = nextId
  nextId += 1
  const tone = input.tone ?? 'info'
  const item: ToastItem = { id, tone, message: input.message }
  if (input.action) {
    item.action = input.action
  }
  items.value = [...items.value.slice(-(MAX_TOASTS - 1)), item]
  if (tone !== 'danger') {
    window.setTimeout(() => dismissToast(id), AUTO_DISMISS_MS)
  }
  return id
}

/** 关闭指定提示 */
export function dismissToast(id: number): void {
  items.value = items.value.filter((item) => item.id !== id)
}
