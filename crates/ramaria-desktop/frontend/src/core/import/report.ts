/**
 * src/core/import/report.ts — 导入报告与结果的展示口径
 *
 * 职责:
 * - 解析报告 / 导入结果的统计条目、成员分布文案与标签翻译
 *
 * 设计特点:
 * - 纯函数，不依赖 Vue / Pinia / Tauri，组件与测试直接调用
 * - 成员按消息数降序、同数按名称升序，保证跨平台稳定顺序
 * - 私聊的对方信息与群聊的成员分布分开表达，不硬凑同一行
 */

import type { ImportAnalysisReport, ImportMemberStat, ImportResult } from '../api/dto'
import type { ImportMetric } from './progress'

/** 成员的展示摘要 */
export interface MemberSummary {
  /** 成员总数 */
  total: number
  /** 截取后的前 N 位 */
  top: { name: string; count: number }[]
  /** 未展示的成员数 */
  rest: number
}

/** 千分位格式化（纯函数，避免受宿主 locale 影响）。 */
export function formatCount(value: number): string {
  const safe = Number.isFinite(value) ? Math.max(0, Math.trunc(value)) : 0
  return String(safe).replace(/\B(?=(\d{3})+(?!\d))/g, ',')
}

/** 成员分布：按消息数降序、同数按名称升序，取前 limit 位。 */
export function memberSummary(
  members: readonly ImportMemberStat[] | null | undefined,
  limit = 5,
): MemberSummary {
  const rows = (members ?? [])
    .filter((member) => !!member)
    .map((member) => ({
      name: member.name || member.uin || member.uid || '未知',
      count: Number.isFinite(member.message_count) ? member.message_count : 0,
    }))
    .sort((left, right) => {
      if (right.count !== left.count) {
        return right.count - left.count
      }
      return left.name.localeCompare(right.name, 'zh-CN')
    })

  const take = Math.max(1, Math.trunc(limit))
  return {
    total: rows.length,
    top: rows.slice(0, take),
    rest: Math.max(0, rows.length - take),
  }
}

/**
 * 成员分布文案。
 *
 * 返回:
 * - 空数组时为空串（私聊或成员数据缺失）。
 */
export function memberText(
  members: readonly ImportMemberStat[] | null | undefined,
  limit = 5,
): string {
  const summary = memberSummary(members, limit)
  if (summary.total === 0) {
    return ''
  }
  const top = summary.top.map((item) => `${item.name}（${formatCount(item.count)} 条）`).join('、')
  return summary.rest > 0 ? `${top} … 等共 ${summary.total} 人` : top
}

/** 对话类型标签 */
export function chatTypeLabel(type: string): string {
  if (type === 'group') {
    return '群聊'
  }
  if (type === 'private') {
    return '私聊'
  }
  return '未知类型'
}

/** 导入模式标签 */
export function modeLabel(mode: string): string {
  if (mode === 'deep') {
    return '深度导入'
  }
  if (mode === 'fast') {
    return '快速导入'
  }
  return mode
}

/** 时间范围文案：空值统一显示「未知」。 */
export function timeRangeText(range: string): string {
  const text = range.trim()
  return text === '' ? '未知' : text
}

/** 解析报告的统计条目（按预览页顺序）。 */
export function reportMetrics(report: ImportAnalysisReport): ImportMetric[] {
  return [
    { key: 'raw', label: '原始消息', value: formatCount(report.total_raw) },
    { key: 'success', label: '成功解析', value: formatCount(report.total_success) },
    { key: 'degraded', label: '降级处理', value: formatCount(report.total_degraded) },
    { key: 'skipped', label: '已跳过', value: formatCount(report.total_skipped) },
    { key: 'sessions', label: '对话 Session', value: formatCount(report.session_count) },
  ]
}

/** 导入结果的统计条目（完成页）。 */
export function resultMetrics(result: ImportResult): ImportMetric[] {
  const metrics: ImportMetric[] = [
    { key: 'sessions', label: '写入 Session', value: formatCount(result.sessions_written) },
    { key: 'messages', label: '写入消息', value: formatCount(result.messages_written) },
    { key: 'mode', label: '导入模式', value: modeLabel(result.mode) },
  ]
  if (result.skipped_count > 0) {
    metrics.push({ key: 'skipped', label: '跳过消息', value: formatCount(result.skipped_count) })
  }
  return metrics
}
