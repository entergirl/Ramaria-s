/**
 * src/core/memory/format.ts — 记忆浏览的展示映射（纯函数）
 *
 * 职责:
 * - 把后端枚举与数值转成界面文案与色调：人格分类、关键词、效价、共享度、事实来源
 *
 * 设计特点:
 * - 只做映射，不读数据、不依赖 store；未知枚举按原值回退，界面不出现空白
 * - 分类与色调由 `kind` + `source` 派生，与人格页的标签口径保持一致
 * - 数值格式化统一保留两位小数，避免同屏精度不一致
 */

import type { PersonaFact, PersonaSummary } from '../api/dto'

/** 标签色调（与 `BaseTag` 的 tone 对齐） */
export type TagTone = 'neutral' | 'brand' | 'accent' | 'ok' | 'warn' | 'danger'

/** 人格分类标签 */
export interface PersonaCategory {
  label: string
  tone: TagTone
}

/**
 * 由人格类型与来源派生分类标签。
 *
 * 规则:
 * - `rama` → 默认人格；`user` → 用户人格
 * - `hist` 或来源为 QQ → 历史导入
 * - 其余角色类型（char / anim / oc）→ 原创角色
 */
export function personaCategory(kind: string, source: string): PersonaCategory {
  if (kind === 'rama') {
    return { label: '默认人格', tone: 'brand' }
  }
  if (kind === 'user') {
    return { label: '用户人格', tone: 'accent' }
  }
  if (kind === 'hist' || source === 'qq') {
    return { label: '历史导入', tone: 'accent' }
  }
  return { label: '原创角色', tone: 'ok' }
}

/** 头像首字：空名返回问号，避免头像空框 */
export function personaInitial(name: string | null | undefined): string {
  const trimmed = (name ?? '').trim()
  return trimmed.length > 0 ? trimmed.slice(0, 1) : '?'
}

/**
 * 人格显示名。
 *
 * 约定:
 * - 用户本人人格统一显示为「你」，与对话页的称呼一致
 * - 缺失时回退 UID，最后回退「未知人格」
 */
export function displayPersonaName(
  uid: string | null | undefined,
  personas: PersonaSummary[],
): string {
  const hit = personas.find((persona) => persona.uid === uid)
  if (hit) {
    return hit.kind === 'user' ? '你' : hit.name
  }
  return uid && uid.length > 0 ? uid : '未知人格'
}

/** 关键词拆分：兼容英文逗号、中文逗号、顿号与分号，去空去重 */
export function splitKeywords(raw: string | null | undefined): string[] {
  if (!raw) {
    return []
  }
  const parts = raw.split(/[,，、;；]/)
  const seen = new Set<string>()
  const result: string[] = []
  for (const part of parts) {
    const word = part.trim()
    if (word.length === 0 || seen.has(word)) {
      continue
    }
    seen.add(word)
    result.push(word)
  }
  return result
}

/** 关键词行最多展示条数（超出丢弃，避免卡片高度失控） */
export const KEYWORD_LIMIT = 8

/** 词条列表（按上限截断） */
export function limitedKeywords(raw: string | null | undefined, limit = KEYWORD_LIMIT): string[] {
  return splitKeywords(raw).slice(0, limit)
}

/** 带符号的两位小数：+0.42 / -0.12 / 0.00 */
export function signedDecimal(value: number | null | undefined): string {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return '—'
  }
  const fixed = value.toFixed(2)
  return value > 0 ? `+${fixed}` : fixed
}

/** 两位小数（无符号）：0.61 */
export function decimal(value: number | null | undefined): string {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return '—'
  }
  return value.toFixed(2)
}

/** 计数量：最多一位小数，整数不补零（12 / 12.5） */
export function amount(value: number | null | undefined): string {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return '—'
  }
  const rounded = Math.round(value * 10) / 10
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1)
}

/** 0..1 转百分比整数：86% */
export function percent(value: number | null | undefined): string {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return '—'
  }
  return `${Math.round(value * 100)}%`
}

/** 效价色调：正向 / 负向 / 中性 */
export function valenceTone(value: number | null | undefined): 'pos' | 'neg' | 'flat' {
  if (typeof value !== 'number' || !Number.isFinite(value) || Math.abs(value) <= 0.3) {
    return 'flat'
  }
  return value > 0 ? 'pos' : 'neg'
}

/** 共享度分档（L2 `share` 为 0..1） */
export function shareLabel(share: number | null | undefined): string {
  if (typeof share !== 'number' || !Number.isFinite(share)) {
    return '未知'
  }
  if (share >= 0.7) {
    return '高'
  }
  return share >= 0.4 ? '中' : '低'
}

/** 陈述方式（L2 `presentation`） */
export function presentationLabel(raw: string | null | undefined): string {
  switch (raw) {
    case 'objective':
      return '客观'
    case 'subjective':
      return '主观'
    case 'mixed':
      return '混合'
    default:
      return raw && raw.length > 0 ? raw : '—'
  }
}

/** 知识事实分层（`tier`） */
export function factTierLabel(raw: string | null | undefined): string {
  switch (raw) {
    case 'stable':
      return '稳定'
    case 'volatile':
      return '动态'
    case 'historical':
      return '历史'
    default:
      return raw && raw.length > 0 ? raw : '稳定'
  }
}

/** 知识事实来源（`source`） */
export function factSourceLabel(raw: string | null | undefined): string {
  switch (raw) {
    case 'event':
      return '事件'
    case 'manual':
      return '人工'
    case 'l1':
      return '会话'
    default:
      return raw && raw.length > 0 ? raw : '未知'
  }
}

/** 画像状态（`status`）的中文短标签 */
export function profileStatusLabel(raw: string | null | undefined): string {
  switch (raw) {
    case 'insufficient':
      return '数据不足'
    case 'preliminary':
      return '初步'
    case 'trusted':
      return '可信'
    default:
      return raw && raw.length > 0 ? raw : '未知'
  }
}

/** 画像状态色调 */
export function profileStatusTone(raw: string | null | undefined): TagTone {
  switch (raw) {
    case 'trusted':
      return 'ok'
    case 'preliminary':
      return 'accent'
    default:
      return 'neutral'
  }
}

/** 置信度色调：≥0.8 强 / ≥0.6 中 / 其余弱 */
export function confidenceTone(value: number | null | undefined): TagTone {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return 'neutral'
  }
  if (value >= 0.8) {
    return 'ok'
  }
  return value >= 0.6 ? 'accent' : 'neutral'
}

/**
 * 知识字段的固定展示顺序。
 *
 * 说明:
 * - 后端以 `HashMap` 返回分组，序列化顺序不稳定，界面必须自行排序
 * - 顺序取 `ProfileField` 的定义顺序，未知字段排在已知字段之后
 */
export const FACT_FIELD_ORDER = [
  '基础信息',
  '近期状态',
  '兴趣爱好',
  '社交情况',
  '历史事件',
  '近期背景',
  '说话风格',
]

/** 知识分组的排序结果（保持后端返回的事实内容，仅重排顺序） */
export interface FactGroup {
  field: string
  facts: PersonaFact[]
}

/** 按固定字段顺序排列知识分组，同字段内按创建时间倒序 */
export function orderFactGroups(grouped: Record<string, PersonaFact[]>): FactGroup[] {
  const fields = Object.keys(grouped)
  const rank = (field: string): number => {
    const index = FACT_FIELD_ORDER.indexOf(field)
    return index === -1 ? FACT_FIELD_ORDER.length : index
  }
  return fields
    .sort((a, b) => {
      const diff = rank(a) - rank(b)
      return diff !== 0 ? diff : a.localeCompare(b, 'zh-Hans-CN')
    })
    .map((field) => ({
      field,
      facts: [...(grouped[field] ?? [])].sort((a, b) => b.created_at - a.created_at),
    }))
}

/** 日期文案：本年省略年份，跨年补年份（不含时刻） */
export function dayLabel(ms: number | null | undefined, now = Date.now()): string {
  if (typeof ms !== 'number' || !Number.isFinite(ms) || ms <= 0) {
    return '未知日期'
  }
  const date = new Date(ms)
  if (Number.isNaN(date.getTime())) {
    return '未知日期'
  }
  const reference = new Date(now)
  const monthDay = `${date.getMonth() + 1}月${date.getDate()}日`
  return date.getFullYear() === reference.getFullYear()
    ? monthDay
    : `${date.getFullYear()}年${monthDay}`
}
