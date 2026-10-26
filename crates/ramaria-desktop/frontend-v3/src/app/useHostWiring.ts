/**
 * src/app/useHostWiring.ts — 宿主接线（通知定位 / 关闭确认）
 *
 * 职责:
 * - 常驻消费 `proactive-message` 的通知点击定位：不在对话视图时切到目标会话
 * - 常驻消费 `close-requested`：暴露确认态供根组件渲染不可关闭的二选一弹窗
 * - 依赖注入的关闭动作：`minimize` 隐藏到托盘，`exit` 退出应用
 *
 * 设计特点:
 * - 在根组件建立，保证事件订阅早于任何视图挂载（视图未在页也不丢定位）
 * - 定位参数走一次性查询 `{sessionId, fromView:'proactive'}`，消息高亮由 chat store 承载
 * - 关闭确认失败只提示并收起弹窗，交由后端超时兜底隐藏到托盘
 */

import { ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'

import { systemApi } from '../core/api'
import { errorMessage } from '../core/errors'
import { EVENTS, useEvent } from '../core/events'
import { pushToast } from '../core/toast'
import { useChatStore } from '../stores/chat'

export function useHostWiring() {
  const router = useRouter()
  const route = useRoute()
  const chat = useChatStore()

  /** 关闭确认是否待处理（不可由用户自行取消） */
  const closeRequested = ref(false)
  /** 关闭动作提交中（防重复点击） */
  const closeBusy = ref(false)

  // 通知点击：视图不在对话页时切过去；会话与消息由 chat store 的定位状态消费
  watch(
    () => chat.activation,
    (value) => {
      if (!value || String(route.name ?? '') === 'chat') {
        return
      }
      void router.push({
        name: 'chat',
        query: { sessionId: value.sessionId, fromView: 'proactive' },
      })
    },
  )

  useEvent(EVENTS.closeRequested, () => {
    closeRequested.value = true
  })

  /** 提交关闭方式；失败收起弹窗并提示（后端超时会隐藏到托盘） */
  async function confirmClose(action: 'minimize' | 'exit'): Promise<void> {
    if (closeBusy.value) {
      return
    }
    closeBusy.value = true
    try {
      await systemApi.confirmCloseAction(action)
    } catch (error) {
      pushToast({ tone: 'danger', message: errorMessage(error, '关闭操作失败') })
    } finally {
      closeBusy.value = false
      closeRequested.value = false
    }
  }

  return {
    closeRequested,
    closeBusy,
    confirmClose,
  }
}
