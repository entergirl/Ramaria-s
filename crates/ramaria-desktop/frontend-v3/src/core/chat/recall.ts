/**
 * src/core/chat/recall.ts — 撤回动作判据与文案（纯函数）
 *
 * 职责:
 * - 判定某条消息此刻能否发起撤回
 * - 生成撤回后的居中系统条文案
 *
 * 设计特点:
 * - 与服务层校验同口径：仅本地会话、本人消息、两分钟窗口内
 * - 只做前端可判定的部分；最终以服务层返回为准，失败文案直接透出
 */
import type { SessionMessage } from '../api/dto'

/** 撤回窗口（毫秒），与服务层一致 */
export const RECALL_WINDOW_MS = 120_000

/** 撤回判据上下文 */
export interface RecallContext {
  /** 当前会话是否只读（社交通道或导入历史） */
  readonly: boolean
  /** 当前时刻（毫秒） */
  now: number
}

/**
 * 是否可以发起撤回。
 *
 * 参数:
 * - `message`: 目标消息
 * - `context`: 只读状态与当前时刻
 */
export function canRecall(message: SessionMessage, context: RecallContext): boolean {
  if (context.readonly) {
    return false
  }
  if (message.role !== 'user' || message.recalled_at !== null) {
    return false
  }
  const elapsed = context.now - message.created_at
  return elapsed >= 0 && elapsed <= RECALL_WINDOW_MS
}

/** 撤回系统条文案 */
export function recallLabel(
  message: SessionMessage,
  personaName: string,
): string {
  if (message.role === 'user') {
    return '你撤回了一条消息'
  }
  if (message.sender_name) {
    return `${message.sender_name}撤回了一条消息`
  }
  return `${personaName || '对方'}撤回了一条消息`
}
