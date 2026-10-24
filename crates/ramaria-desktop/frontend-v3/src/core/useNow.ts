/**
 * src/core/useNow.ts — 定时刷新当前时刻
 *
 * 职责:
 * - 为相对时间文案与撤回窗口提供一个按固定间隔更新的响应式时刻
 *
 * 设计特点:
 * - 组件卸载时清理定时器，不留后台任务
 * - 间隔默认 30 秒：足以覆盖「刚刚 / 分钟」与两分钟窗口的判定精度
 */
import { onScopeDispose, ref, type Ref } from 'vue'

/** 创建自动推进的当前时刻（毫秒） */
export function useNow(intervalMs = 30_000): Ref<number> {
  const now = ref(Date.now())
  const timer = window.setInterval(() => {
    now.value = Date.now()
  }, intervalMs)

  onScopeDispose(() => {
    window.clearInterval(timer)
  })

  return now
}
