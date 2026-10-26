/**
 * src/core/indexStatus.ts — 索引状态的界面口径
 *
 * 职责:
 * - 把 `IndexStatusView` 映射为设置卡共用的状态文案、色调与覆盖数
 *
 * 设计特点:
 * - 只做展示映射，不推断后端没有给出的状态
 * - 未读取到时统一按「未知」处理，调用方不各自兜底
 */

import type { IndexStatusView } from './api/dto'

/** 索引状态色调（与 BaseTag 的语义档一致） */
export type IndexStatusTone = 'neutral' | 'ok' | 'warn' | 'danger'

/** 状态一句话文案 */
export function indexStatusText(status: IndexStatusView | null): string {
  if (!status) {
    return '未知'
  }
  switch (status.status) {
    case 'idle':
      return '未建立'
    case 'building':
      return `构建中 ${status.percent}%`
    case 'ready':
      return status.failed > 0 ? `已就绪 · ${status.failed} 条待补` : '已就绪'
    default:
      return '构建失败'
  }
}

/** 状态色调 */
export function indexStatusTone(status: IndexStatusView | null): IndexStatusTone {
  if (!status) {
    return 'neutral'
  }
  switch (status.status) {
    case 'ready':
      return status.failed > 0 ? 'warn' : 'ok'
    case 'building':
      return 'neutral'
    case 'degraded':
      return 'danger'
    default:
      return 'warn'
  }
}

/** 向量覆盖数：`已算 / 总量 条` */
export function indexCoverageText(status: IndexStatusView | null): string {
  if (!status) {
    return '读取中…'
  }
  return `${status.loaded} / ${status.total} 条`
}

/** 嵌入模型标识；未建立或后端未给出时给可读占位 */
export function indexModelText(status: IndexStatusView | null): string {
  return status?.model ?? '未记录'
}
