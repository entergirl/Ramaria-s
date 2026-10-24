/**
 * src/core/chat/time.ts — 对话时间显示（纯函数）
 *
 * 职责:
 * - 统一会话列表、消息分隔条与气泡元数据的时间文案
 *
 * 设计特点:
 * - 全部以 Unix 毫秒为入参，按本地时区计算自然日边界
 * - 相对时间只用于近一周，超出后回到绝对日期，避免「387天前」
 * - 非法时间戳统一返回「未知时间」，不抛错、不出现 NaN
 */

const MINUTE_MS = 60_000
const HOUR_MS = 3_600_000
const DAY_MS = 86_400_000

function toDate(ms: number): Date | null {
  if (!Number.isFinite(ms) || ms <= 0) {
    return null
  }
  const date = new Date(ms)
  return Number.isNaN(date.getTime()) ? null : date
}

function pad2(value: number): string {
  return value < 10 ? `0${value}` : String(value)
}

/** 两个时间戳之间的自然日差（同一天为 0） */
export function dayDiff(earlier: number, later: number): number {
  const a = toDate(earlier)
  const b = toDate(later)
  if (!a || !b) {
    return 0
  }
  const startOfA = new Date(a.getFullYear(), a.getMonth(), a.getDate()).getTime()
  const startOfB = new Date(b.getFullYear(), b.getMonth(), b.getDate()).getTime()
  return Math.round((startOfB - startOfA) / DAY_MS)
}

/** 是否同一天 */
export function isSameDay(a: number, b: number): boolean {
  return dayDiff(a, b) === 0
}

/** 本地自然日键（`YYYY-MM-DD`），用于消息列表日期分隔 */
export function dayKey(ms: number): string {
  const date = toDate(ms)
  if (!date) {
    return ''
  }
  return `${date.getFullYear()}-${pad2(date.getMonth() + 1)}-${pad2(date.getDate())}`
}

/** `HH:mm` */
export function formatClock(ms: number): string {
  const date = toDate(ms)
  if (!date) {
    return '未知时间'
  }
  return `${pad2(date.getHours())}:${pad2(date.getMinutes())}`
}

/** `M月D日 HH:mm`（跨年补年份） */
export function formatDateTime(ms: number, now = Date.now()): string {
  const date = toDate(ms)
  const reference = toDate(now)
  if (!date || !reference) {
    return '未知时间'
  }
  const clock = formatClock(ms)
  const sameYear = date.getFullYear() === reference.getFullYear()
  const day = `${date.getMonth() + 1}月${date.getDate()}日`
  return sameYear ? `${day} ${clock}` : `${date.getFullYear()}年${day} ${clock}`
}

/** 会话列表时间：近一周相对、更早绝对日期 */
export function smartTime(ms: number, now = Date.now()): string {
  const date = toDate(ms)
  const reference = toDate(now)
  if (!date || !reference) {
    return '未知时间'
  }

  const diff = reference.getTime() - date.getTime()
  if (diff < 0) {
    return formatDateTime(ms, now)
  }
  if (diff < MINUTE_MS) {
    return '刚刚'
  }
  if (diff < HOUR_MS) {
    return `${Math.floor(diff / MINUTE_MS)}分钟前`
  }
  if (diff < DAY_MS) {
    return `${Math.floor(diff / HOUR_MS)}小时前`
  }

  const days = dayDiff(ms, now)
  if (days <= 0) {
    return `${Math.floor(diff / HOUR_MS)}小时前`
  }
  if (days === 1) {
    return `昨天 ${formatClock(ms)}`
  }
  if (days < 7) {
    return `${days}天前`
  }

  const sameYear = date.getFullYear() === reference.getFullYear()
  const monthDay = `${date.getMonth() + 1}月${date.getDate()}日`
  return sameYear ? monthDay : `${date.getFullYear()}年${monthDay}`
}

/** 消息列表日期分隔条：今天 / 昨天 / 月日 / 年月日 */
export function dayDivider(ms: number, now = Date.now()): string {
  const date = toDate(ms)
  const reference = toDate(now)
  if (!date || !reference) {
    return '未知日期'
  }
  const days = dayDiff(ms, now)
  if (days === 0) {
    return '今天'
  }
  if (days === 1) {
    return '昨天'
  }
  const day = `${date.getMonth() + 1}月${date.getDate()}日`
  return date.getFullYear() === reference.getFullYear()
    ? day
    : `${date.getFullYear()}年${day}`
}
