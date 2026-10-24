/**
 * src/core/persona/format.ts — 人格视图的展示映射（纯函数）
 *
 * 职责:
 * - 把人格类型、预设模式、拆解状态与导入导出统计转成界面文案
 *
 * 设计特点:
 * - 只做映射：未知枚举按原值回退，界面不出现空白，也不猜测语义
 * - 导入预览正文由服务层渲染，本模块只处理视图自带的少量结构化字段
 * - 统计文案统一「类别 数量」并列，导入与导出共用同一套书写顺序
 */

import type {
  DecomposeExecution,
  ImportWriteStats,
  PersonaExportReceipt,
  PersonaExportStats,
  PresetImportOutcome,
} from '../api/dto'
import type { TagTone } from '../memory/format'

/** 人格类型的中文释义（标签仍显示原始枚举） */
const KIND_LABELS: Record<string, string> = {
  user: '用户自身',
  rama: '本地人格',
  char: '角色人格',
  anim: '动画角色',
  oc: '原创角色',
  hist: '历史导入',
}

/** 类型释义：未知类型回退原值，空值回退「未知类型」 */
export function personaKindLabel(kind: string): string {
  const hit = KIND_LABELS[kind]
  if (hit) {
    return hit
  }
  return kind && kind.length > 0 ? kind : '未知类型'
}

/** 类型标签色调：产品自身品牌色、用户侧辅助色、导入与角色中性/成功色 */
export function personaKindTone(kind: string): TagTone {
  switch (kind) {
    case 'rama':
      return 'brand'
    case 'user':
      return 'accent'
    case 'hist':
      return 'accent'
    case 'char':
    case 'anim':
    case 'oc':
      return 'ok'
    default:
      return 'neutral'
  }
}

/** 预设导入模式：create / merge */
export function presetModeLabel(mode: string): string {
  switch (mode) {
    case 'create':
      return '新建'
    case 'merge':
      return '合并'
    default:
      return mode && mode.length > 0 ? mode : '未知模式'
  }
}

/** 拆解跳过原因的中文说明 */
export function decomposeSkipLabel(reason: string | undefined): string {
  switch (reason) {
    case 'privacy_unconfirmed':
      return '未确认隐私范围'
    case 'no_input':
      return '没有可拆解的文本'
    case 'llm_failed':
      return '对话模型不可用'
    default:
      return reason && reason.length > 0 ? reason : '未说明原因'
  }
}

/** 拆解执行情况的一句话 */
export function decomposeText(decompose: DecomposeExecution): string {
  if (!decompose.requested) {
    return '未启用'
  }
  if (decompose.executed) {
    return `已执行（${decompose.fact_count} 条事实 / ${decompose.trait_count} 条特质）`
  }
  return `跳过：${decomposeSkipLabel(decompose.skipped_reason)}`
}

/** 写入统计的一句话 */
export function writeStatsText(written: ImportWriteStats): string {
  const parts = [
    `特质 ${written.traits}`,
    `事实 ${written.facts}`,
    `规则 ${written.behavior_rules}`,
    `示例 ${written.examples}`,
  ]
  if (written.failed > 0) {
    parts.push(`失败 ${written.failed}`)
  }
  return parts.join(' / ')
}

/** 导入结果提示：名称、uid、模式、写入统计与拆解情况 */
export function importOutcomeText(outcome: PresetImportOutcome): string {
  return `已导入「${outcome.name}」（${outcome.uid} · ${presetModeLabel(outcome.mode)}）：${writeStatsText(outcome.written)}；拆解${decomposeText(outcome.decompose)}`
}

/** 导出统计的一句话 */
export function exportStatsText(stats: PersonaExportStats): string {
  const parts = [
    `特质 ${stats.traits}`,
    `事实 ${stats.facts}`,
    `规则 ${stats.behaviorRules}`,
    `自由文本块 ${stats.blocks}`,
  ]
  if (stats.promptPatch) {
    parts.push('含提示词补丁')
  }
  parts.push(`示例 ${stats.examplesIncluded}`)
  return parts.join(' / ')
}

/** 导出结果提示：写入路径与统计 */
export function exportReceiptText(receipt: PersonaExportReceipt): string {
  return `已导出到 ${receipt.writtenTo}：${exportStatsText(receipt.stats)}`
}
