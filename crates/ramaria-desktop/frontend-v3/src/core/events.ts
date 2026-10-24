/**
 * src/core/events.ts — 桌面事件订阅
 *
 * 职责:
 * - 8 个桌面事件的类型化订阅，并在作用域销毁时自动取消
 *
 * 设计特点:
 * - 事件名是全仓唯一出现事件字符串的位置，与 Rust `EVENT_*` 常量一一对应
 * - 非宿主环境或宿主未注入 event 时静默跳过，不向界面抛错
 * - 订阅取消以卸载 / 作用域销毁为准，晚到的取消函数会立即执行，避免重复监听
 * - 导入与通道事件的负载形状随 M6 / M7 视图补齐，当前只锁事件名
 */

import { getCurrentScope, onScopeDispose } from 'vue'

import type {
  ChatDeltaPayload,
  ChatDonePayload,
  ChatErrorPayload,
  IndexProgress,
  ModelDownloadProgress,
  ProactiveMessagePayload,
} from './api/dto'
import { getTauriRuntime } from './transport'

/** 事件名常量 */
export const EVENTS = {
  chatDelta: 'chat-delta',
  chatDone: 'chat-done',
  chatError: 'chat-error',
  importProgress: 'import-progress',
  proactiveMessage: 'proactive-message',
  channelStatus: 'channel-status',
  closeRequested: 'close-requested',
  indexProgress: 'index-progress',
  modelDownloadProgress: 'model-download-progress',
} as const

/** 事件名联合类型 */
export type EventName = (typeof EVENTS)[keyof typeof EVENTS]

/** 事件名 → 负载形状 */
export interface EventPayloadMap {
  'chat-delta': ChatDeltaPayload
  'chat-done': ChatDonePayload
  'chat-error': ChatErrorPayload
  'import-progress': Record<string, unknown>
  'proactive-message': ProactiveMessagePayload
  'channel-status': Record<string, unknown>
  'close-requested': null
  'index-progress': IndexProgress
  'model-download-progress': ModelDownloadProgress
}

/**
 * 订阅一个事件，返回取消函数。
 *
 * 说明:
 * - 非宿主环境返回空取消函数，调用方可无条件使用
 */
export async function subscribe<K extends EventName>(
  event: K,
  handler: (payload: EventPayloadMap[K]) => void,
): Promise<() => void> {
  const runtime = getTauriRuntime()
  if (!runtime?.event) {
    return () => {}
  }
  return runtime.event.listen<EventPayloadMap[K]>(event, (incoming) => {
    handler(incoming.payload)
  })
}

/**
 * 在当前组件 / 效果作用域内订阅事件。
 *
 * 说明:
 * - 作用域销毁时自动取消；无作用域（模块级调用）时退化为常驻订阅
 */
export function useEvent<K extends EventName>(
  event: K,
  handler: (payload: EventPayloadMap[K]) => void,
): void {
  let disposed = false
  let unlisten: (() => void) | null = null

  void subscribe(event, (payload) => {
    if (!disposed) {
      handler(payload)
    }
  })
    .then((off) => {
      if (disposed) {
        off()
        return
      }
      unlisten = off
    })
    .catch(() => {
      // 订阅失败按无事件处理，不阻塞界面
    })

  if (getCurrentScope()) {
    onScopeDispose(() => {
      disposed = true
      unlisten?.()
      unlisten = null
    })
  }
}
