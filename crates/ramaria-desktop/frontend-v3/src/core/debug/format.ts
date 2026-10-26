/**
 * src/core/debug/format.ts — 调试视图的展示换算（纯函数）
 *
 * 职责:
 * - 把关键词池三态、说话风格统计参数与产物文件元信息换算成可直接渲染的视图数据
 *
 * 设计特点:
 * - 只做确定性的整理与格式化，不读 store、不发命令、不含 DOM
 * - 统计参数按五维分组呈现（口癖词 / 句式句长 / 标点 / 情感表达 / 话题词汇）
 * - 计数字段以每千字频率补齐量纲，字符数为 0 时只显示原始次数
 * - 词频类条目按展示上限截断并给出剩余条数，避免卡内无限增高
 */

import type { EvalFile, KeywordEntry, StyleStats } from '../api/dto'

// =========================================================
// 关键词池三态
// =========================================================

/** 三态元信息：展示顺序、标题与标签色调 */
export interface KeywordStatusMeta {
  title: string
  tone: 'brand' | 'accent' | 'warn'
  /** 分组默认展开 */
  openByDefault: boolean
}

/** 三态元信息表（顺序即展示顺序） */
export const KEYWORD_STATUS_META: Record<string, KeywordStatusMeta> = {
  canonical: { title: '规范词', tone: 'brand', openByDefault: true },
  alias: { title: '已确认别名', tone: 'accent', openByDefault: false },
  pending: { title: '待确认别名', tone: 'warn', openByDefault: false },
}

/** 一组词条 */
export interface KeywordGroup {
  status: string
  title: string
  tone: KeywordStatusMeta['tone']
  openByDefault: boolean
  items: KeywordEntry[]
}

/**
 * 按三态分组词条。
 *
 * 参数:
 * - `entries`: 关键词池全量词条
 *
 * 返回:
 * - 固定三组（canonical / alias / pending），空组保留以便展示计数与展开位
 *
 * 说明:
 * - 未知状态归入待确认组，避免词条在界面上凭空消失
 */
export function groupKeywords(entries: KeywordEntry[]): KeywordGroup[] {
  const buckets = new Map<string, KeywordEntry[]>([
    ['canonical', []],
    ['alias', []],
    ['pending', []],
  ])
  for (const entry of entries) {
    const key = buckets.has(entry.status) ? entry.status : 'pending'
    buckets.get(key)?.push(entry)
  }
  return Object.entries(KEYWORD_STATUS_META).map(([status, meta]) => ({
    status,
    title: meta.title,
    tone: meta.tone,
    openByDefault: meta.openByDefault,
    items: buckets.get(status) ?? [],
  }))
}

// =========================================================
// 说话风格：五维统计
// =========================================================

/** 单条统计字段 */
export interface StyleField {
  label: string
  value: string
}

/** 词频条目 */
export interface StyleChip {
  text: string
  count: number
}

/** 一个统计维度 */
export interface StyleGroup {
  id: string
  title: string
  fields: StyleField[]
  chips: StyleChip[]
}

/** 五维统计的解析结果 */
export interface StyleStatsView {
  groups: StyleGroup[]
  /** 统计参数能否解析（false 表示 stats_json 损坏，只展示样本与规则文本） */
  parsable: boolean
}

/** 词频类条目的展示上限 */
const CHIP_LIMIT = 12

/**
 * 解析 `stats_json` 为五维分组。
 *
 * 参数:
 * - `raw`: 后端返回的统计参数 JSON 文本
 *
 * 返回:
 * - 五组固定顺序的维度视图；解析失败时 `parsable=false` 且分组为空
 */
export function parseStyleStats(raw: string): StyleStatsView {
  const data = parseObject(raw)
  if (!data) {
    return { groups: [], parsable: false }
  }
  const totalChars = numberOrZero(data.total_chars)
  const groups: StyleGroup[] = [
    {
      id: 'words',
      title: '口癖词',
      fields: [],
      chips: frequencyChips(data.word_freq),
    },
    {
      id: 'sentence',
      title: '句式句长',
      fields: compact([
        field('平均句长', decimalText(data.sentence_len_mean, 1, ' 字')),
        field('句长 P25', decimalText(data.sentence_len_p25, 1, ' 字')),
        field('句长 P75', decimalText(data.sentence_len_p75, 1, ' 字')),
        field('总字符', countText(data.total_chars)),
        field('总句数', countText(data.total_sentences)),
      ]),
      chips: [],
    },
    {
      id: 'punct',
      title: '标点',
      fields: compact([
        field('断句符 ||', counted(data.slash_count, totalChars)),
        field('逗号', counted(data.comma_count, totalChars)),
        field('换行', counted(data.newline_count, totalChars)),
        field('感叹号', counted(data.exclaim_count, totalChars)),
        field('问号', counted(data.question_count, totalChars)),
        field('省略号', counted(data.ellipsis_count, totalChars)),
        field('括号', counted(data.paren_count, totalChars)),
        field('波浪号', counted(data.tilde_count, totalChars)),
      ]),
      chips: [],
    },
    {
      id: 'emotion',
      title: '情感表达',
      fields: compact([
        field('情绪均值', decimalText(data.sentiment_mean, 2)),
        field('情绪标准差', decimalText(data.sentiment_std, 2)),
        field('情绪样本', countText(data.sentiment_n)),
        field('语气词次数', counted(data.interjection_count, totalChars)),
        field('情绪词消息', countText(data.sentiment_word_messages)),
        field('情绪词命中率', rateText(data.sentiment_word_messages, data.sample_count)),
      ]),
      chips: [],
    },
    {
      id: 'topics',
      title: '话题词汇',
      fields: [],
      chips: frequencyChips(data.topic_freq),
    },
  ]
  const visible = groups.filter(
    (group) => group.fields.length > 0 || group.chips.length > 0,
  )
  return { groups: visible, parsable: true }
}

/** 统计状态色调：ready 正常，其余提示样本不足或无显著项 */
export function styleStatusTone(status: string): 'ok' | 'warn' | 'neutral' {
  if (status === 'ready') {
    return 'ok'
  }
  if (status === 'insufficient' || status === 'no_significant') {
    return 'warn'
  }
  return 'neutral'
}

/** 统计样本量的补充说明（阈值判断由后端给出，这里只解释状态） */
export function styleStatusHint(status: string): string {
  if (status === 'insufficient') {
    return '样本低于阈值，仅积累参数，不生成规则'
  }
  if (status === 'no_significant') {
    return '样本充足但无显著项，不生成规则'
  }
  return ''
}

/**
 * 统计记录的时间文案。
 *
 * 参数:
 * - `ms`: 更新时间（Unix 毫秒）
 * - `now`: 参照时间，便于用例固定「今年」判断
 */
export function updatedLabel(ms: number, now = Date.now()): string {
  if (!Number.isFinite(ms) || ms <= 0) {
    return '未知时间'
  }
  return `${dateLabel(ms, now)} ${clockLabel(ms)}`
}

/** 说话风格视图的完整包装（样本量 / 状态 / 来源 + 五维） */
export interface StyleStatsModel {
  sampleCount: number
  status: string
  statusLabel: string
  statusTone: 'ok' | 'warn' | 'neutral'
  statusHint: string
  ruleSourceLabel: string
  baselineVersion: number
  ruleText: string
  updatedAt: string
  groups: StyleGroup[]
  parsable: boolean
}

/** 把后端统计视图整理为展示模型 */
export function buildStyleModel(stats: StyleStats): StyleStatsModel {
  const parsed = parseStyleStats(stats.stats_json)
  return {
    sampleCount: stats.sample_count,
    status: stats.status,
    statusLabel: stats.status_label || stats.status,
    statusTone: styleStatusTone(stats.status),
    statusHint: styleStatusHint(stats.status),
    ruleSourceLabel: stats.rule_source_label || stats.rule_source,
    baselineVersion: stats.baseline_version,
    ruleText: (stats.rule_text ?? '').trim(),
    updatedAt: updatedLabel(stats.updated_at),
    groups: parsed.groups,
    parsable: parsed.parsable,
  }
}

// =========================================================
// 评估产物文件
// =========================================================

/** 文件大小文案：B / KB / MB，保留一位小数 */
export function fileSizeLabel(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) {
    return '—'
  }
  if (bytes < 1024) {
    return `${bytes} B`
  }
  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} KB`
  }
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`
}

/** 产物文件副标题：大小 + 修改时间 */
export function evalFileMeta(file: EvalFile, now = Date.now()): string {
  const parts = [fileSizeLabel(file.size)]
  if (file.modified_at !== null && file.modified_at > 0) {
    parts.push(`${dateLabel(file.modified_at, now)} ${clockLabel(file.modified_at)}`)
  }
  return parts.join(' · ')
}

/** 分数文案：最多三位小数，去掉无意义的末尾零 */
export function scoreLabel(value: number): string {
  if (!Number.isFinite(value)) {
    return '—'
  }
  return String(Math.round(value * 1000) / 1000)
}

/** 比例文案：0..1 转百分比一位小数 */
export function ratioLabel(value: number): string {
  if (!Number.isFinite(value)) {
    return '—'
  }
  return `${(value * 100).toFixed(1)}%`
}

// =========================================================
// 私有辅助
// =========================================================

/** 解析为普通对象；非对象与解析失败都返回 null */
function parseObject(raw: string): Record<string, unknown> | null {
  if (!raw || typeof raw !== 'string') {
    return null
  }
  try {
    const value: unknown = JSON.parse(raw)
    if (!value || typeof value !== 'object' || Array.isArray(value)) {
      return null
    }
    return value as Record<string, unknown>
  } catch {
    return null
  }
}

function field(label: string, value: string | null): StyleField | null {
  return value === null ? null : { label, value }
}

/** 去掉取值为空的字段 */
function compact(fields: Array<StyleField | null>): StyleField[] {
  return fields.filter((item): item is StyleField => item !== null)
}

function numberOrZero(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : 0
}

function countText(value: unknown): string | null {
  return typeof value === 'number' && Number.isFinite(value) ? String(value) : null
}

/** 小数字段：固定位数（整数则不补零），可选单位 */
function decimalText(value: unknown, digits: number, unit = ''): string | null {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return null
  }
  const text = Number.isInteger(value) ? String(value) : value.toFixed(digits)
  return `${text}${unit}`
}

/** 计数字段：次数 + 每千字频率 */
function counted(value: unknown, totalChars: number): string | null {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return null
  }
  if (totalChars <= 0) {
    return `${value} 次`
  }
  return `${value} 次 · ${((value / totalChars) * 1000).toFixed(1)}‰`
}

/** 比率字段：分子 / 分母 */
function rateText(numerator: unknown, denominator: unknown): string | null {
  if (typeof numerator !== 'number' || typeof denominator !== 'number') {
    return null
  }
  if (denominator <= 0) {
    return null
  }
  return ratioLabel(numerator / denominator)
}

/** 词频条目：`[["词", 3], ...]` 截断到展示上限 */
function frequencyChips(value: unknown): StyleChip[] {
  if (!Array.isArray(value)) {
    return []
  }
  const chips: StyleChip[] = []
  for (const item of value) {
    if (!Array.isArray(item) || item.length < 2) {
      continue
    }
    const text = typeof item[0] === 'string' ? item[0] : String(item[0])
    const count = typeof item[1] === 'number' ? item[1] : Number(item[1])
    if (!text || !Number.isFinite(count)) {
      continue
    }
    chips.push({ text, count })
    if (chips.length >= CHIP_LIMIT) {
      break
    }
  }
  return chips
}

/** 日期文案：本年省略年份 */
function dateLabel(ms: number, now: number): string {
  const date = new Date(ms)
  const reference = new Date(now)
  const text = `${date.getMonth() + 1}月${date.getDate()}日`
  return date.getFullYear() === reference.getFullYear()
    ? text
    : `${date.getFullYear()}年${text}`
}

/** 时刻文案：HH:mm */
function clockLabel(ms: number): string {
  const date = new Date(ms)
  const hour = String(date.getHours()).padStart(2, '0')
  const minute = String(date.getMinutes()).padStart(2, '0')
  return `${hour}:${minute}`
}
