/**
 * src/core/rules/format.ts — 规则浏览的展示映射（纯函数）
 *
 * 职责:
 * - 把行为规则的枚举与数值转成界面文案：来源、启停、候选标记、关键词、禁忌词
 * - 提供编辑草稿的归一化与「是否有改动」判定
 *
 * 设计特点:
 * - 只做映射与比较，不读数据、不依赖 store；未知取值按原值回退，界面不出现空白
 * - 禁忌词兼容中文逗号、顿号与分号，与用户的输入习惯一致
 * - 候选规则（无规则文本）的展示文案只在此处定义，卡片与详情页共用
 */

import type { BehaviorRuleView, RuleEvidenceItem } from '../api/dto'
import { KEYWORD_LIMIT, splitKeywords, type TagTone } from '../memory/format'

/** 候选规则的占位文案（无规则文本，只做参数注入） */
export const CANDIDATE_TEXT = '（候选规则，仅参数注入）'

/** 规则来源标签：手工 / 自动 */
export function ruleSourceLabel(source: string): string {
  return source === 'manual' ? '手工' : '自动'
}

/** 规则来源色调：手工用辅色（强锚点），自动用成功色 */
export function ruleSourceTone(source: string): TagTone {
  return source === 'manual' ? 'accent' : 'ok'
}

/** 启停状态标签 */
export function ruleStatusLabel(enabled: boolean): string {
  return enabled ? '已启用' : '已停用'
}

/** 是否为候选规则：`reaction` 缺失或只有空白 */
export function isCandidateRule(rule: BehaviorRuleView): boolean {
  return (rule.reaction ?? '').trim().length === 0
}

/** 规则卡片正文：候选规则给固定占位，不留空白 */
export function ruleText(rule: BehaviorRuleView): string {
  const text = (rule.reaction ?? '').trim()
  return text.length > 0 ? text : CANDIDATE_TEXT
}

/** 词条归一：去空、去重、保序、按上限截断 */
function dedupeWords(words: string[], limit: number): string[] {
  const seen = new Set<string>()
  const result: string[] = []
  for (const raw of words) {
    const word = raw.trim()
    if (word.length === 0 || seen.has(word)) {
      continue
    }
    seen.add(word)
    result.push(word)
    if (result.length >= limit) {
      break
    }
  }
  return result
}

/** 情境关键词（去空去重后按上限截断） */
export function ruleKeywords(rule: BehaviorRuleView, limit = KEYWORD_LIMIT): string[] {
  return dedupeWords(rule.situation.keywords, limit)
}

/** 禁忌词归一（不限条数，只去空去重） */
export function normalizeAvoid(avoid: string[]): string[] {
  return dedupeWords(avoid, Number.POSITIVE_INFINITY)
}

/** 禁忌词输入归一化：兼容中文逗号与顿号，去空保序 */
export function parseAvoidInput(raw: string): string[] {
  return splitKeywords(raw)
}

/** 禁忌词回填到输入框 */
export function avoidText(avoid: string[]): string {
  return avoid.join(',')
}

/** 证据项关键词（后端以逗号串返回，可能为空） */
export function evidenceKeywords(item: RuleEvidenceItem, limit = KEYWORD_LIMIT): string[] {
  return splitKeywords(item.keywords).slice(0, limit)
}

/** 编辑草稿是否有改动（文本按 trim 比较，禁忌词按归一化列表比较） */
export function ruleEditChanged(
  rule: BehaviorRuleView,
  reaction: string,
  avoid: string,
): boolean {
  const currentReaction = (rule.reaction ?? '').trim()
  if (reaction.trim() !== currentReaction) {
    return true
  }
  return parseAvoidInput(avoid).join(',') !== rule.avoid.join(',')
}

/**
 * 规则文本的校验结果。
 *
 * 约定:
 * - 已有文本的规则不允许清空（后端同样拒绝），返回一句原因
 * - 候选规则可留空，保存时只提交禁忌词
 */
export function reactionError(rule: BehaviorRuleView, reaction: string): string {
  if (reaction.trim().length > 0) {
    return ''
  }
  return (rule.reaction ?? '').trim().length > 0 ? '规则文本不能为空' : ''
}
