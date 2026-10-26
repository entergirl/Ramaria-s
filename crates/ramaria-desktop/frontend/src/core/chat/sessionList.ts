/**
 * src/core/chat/sessionList.ts — 会话卡片的行构造、筛选与分组（纯函数）
 *
 * 职责:
 * - 把会话摘要 + 人格信息派生为会话行：显示名、首字头像、来源标签、副行、时间与未读
 * - 提供搜索过滤与「今天 / 更早」分组
 *
 * 设计特点:
 * - 来源与只读口径与对话视图一致：社交通道只读、QQ 导入只读
 * - 副行优先取后端最后一条预览，缺字段时回退消息数，不使用占位造假数据
 * - 不读全局状态，人格信息由调用方按 uid 解析
 */
import type { SessionSummary } from '../api/dto'
import { isSameDay, smartTime } from './time'

/** 标签色调（与 BaseTag 的 tone 对齐） */
export type TagTone = 'neutral' | 'brand' | 'accent' | 'ok' | 'warn' | 'danger'

/** 会话行标签 */
export interface SessionTag {
  label: string
  tone: TagTone
}

/** 会话渲染所需的人格摘要 */
export interface PersonaBrief {
  uid: string
  name: string
  kind: string
  source: string
}

/** 会话行（会话卡片的一行） */
export interface SessionRow {
  session: SessionSummary
  name: string
  initial: string
  tags: SessionTag[]
  preview: string
  timeText: string
  unread: number
  imported: boolean
  readonly: boolean
}

/** 分组结果 */
export interface SessionGroups {
  today: SessionRow[]
  earlier: SessionRow[]
}

/** 通道显示名（本地不标注） */
export function channelLabel(channel: string): string {
  const normalized = (channel || '').trim().toLowerCase()
  if (!normalized || normalized === 'local') {
    return ''
  }
  if (normalized === 'qq') {
    return 'QQ'
  }
  if (normalized === 'telegram') {
    return 'Telegram'
  }
  if (normalized === 'mcp') {
    return 'MCP'
  }
  return normalized.toUpperCase()
}

/** 会话来源是否为导入口径（归属人格来源为 QQ） */
export function isImportedSession(persona: PersonaBrief | null | undefined): boolean {
  return persona?.source === 'qq'
}

/** 会话是否只读：社交通道或导入历史 */
export function isReadonlySession(
  channel: string,
  persona: PersonaBrief | null | undefined,
): boolean {
  return (channel || 'local') !== 'local' || isImportedSession(persona)
}

/** 会话显示名：人格名优先，缺失时用会话短标识 */
export function sessionDisplayName(
  session: SessionSummary,
  persona: PersonaBrief | null | undefined,
): string {
  if (persona?.name) {
    return persona.name
  }
  return `会话 ${session.id.slice(0, 8)}`
}

/** 副行文案：最后一条预览优先，缺字段时回退消息数 */
export function sessionPreview(session: SessionSummary): string {
  const preview = session.last_message_preview?.trim()
  if (preview) {
    return preview
  }
  return session.message_count > 0 ? `${session.message_count} 条消息` : '无消息'
}

/**
 * 会话标签：默认人格 / 导入 / 来源通道 / 只读。
 *
 * 参数:
 * - `channel`: 会话来源通道
 * - `persona`: 会话归属人格
 */
export function sessionTags(
  channel: string,
  persona: PersonaBrief | null | undefined,
): SessionTag[] {
  const tags: SessionTag[] = []
  if (persona?.kind === 'rama') {
    tags.push({ label: '默认人格', tone: 'brand' })
  }
  if (isImportedSession(persona)) {
    tags.push({ label: '导入', tone: 'accent' })
  }
  const label = channelLabel(channel)
  if (label) {
    tags.push({ label, tone: 'accent' })
  }
  if (isReadonlySession(channel, persona)) {
    tags.push({ label: '只读', tone: 'warn' })
  }
  return tags
}

/**
 * 构造单行会话。
 *
 * 参数:
 * - `session`: 会话摘要
 * - `persona`: 会话归属人格（缺失时按无名会话渲染）
 * - `now`: 参考时间（毫秒）
 */
export function buildSessionRow(
  session: SessionSummary,
  persona: PersonaBrief | null | undefined,
  now: number,
): SessionRow {
  const name = sessionDisplayName(session, persona)
  const tags = sessionTags(session.channel, persona)

  return {
    session,
    name,
    initial: name.slice(0, 1),
    tags,
    preview: sessionPreview(session),
    timeText: smartTime(session.started_at, now),
    unread: Math.max(0, session.unread || 0),
    imported: isImportedSession(persona),
    readonly: isReadonlySession(session.channel, persona),
  }
}

/** 构造全部会话行（按开始时间倒序） */
export function buildSessionRows(
  sessions: SessionSummary[],
  resolvePersona: (uid: string | null) => PersonaBrief | null | undefined,
  now: number,
): SessionRow[] {
  return sessions
    .map((session) => buildSessionRow(session, resolvePersona(session.persona_uid), now))
    .sort((a, b) => b.session.started_at - a.session.started_at)
}

/** 关键词过滤：显示名 / 副行 / 时间 / 人格标识 / 会话短标识 */
export function filterSessionRows(rows: SessionRow[], keyword: string): SessionRow[] {
  const needle = keyword.trim().toLowerCase()
  if (!needle) {
    return rows
  }
  return rows.filter((row) => {
    const haystack = [
      row.name,
      row.preview,
      row.timeText,
      row.session.persona_uid ?? '',
      row.session.id.slice(0, 8),
    ]
      .join(' ')
      .toLowerCase()
    return haystack.includes(needle)
  })
}

/** 按开始时间分「今天 / 更早」 */
export function groupSessionRows(rows: SessionRow[], now: number): SessionGroups {
  const groups: SessionGroups = { today: [], earlier: [] }
  for (const row of rows) {
    if (isTodayLocal(row.session.started_at, now)) {
      groups.today.push(row)
    } else {
      groups.earlier.push(row)
    }
  }
  return groups
}

/**
 * 是否今天。
 *
 * 说明:
 * - 与列表时间文案同源（本地自然日），避免「今天」分组里出现「昨天」字样
 */
export function isTodayLocal(ms: number, now: number): boolean {
  return isSameDay(ms, now)
}
