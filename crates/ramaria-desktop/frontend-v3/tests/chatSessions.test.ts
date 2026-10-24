/**
 * tests/chatSessions.test.ts — 会话列表、撤回判据与消息合并
 *
 * 覆盖:
 * - 会话行的标签、副行、时间与只读判据
 * - 搜索过滤与「今天 / 更早」分组
 * - 撤回可判定条件与系统条文案
 * - 服务端消息页合并与去重追加
 * - 命令回执解析与时间文案
 */
import { describe, expect, it } from 'vitest'

import type { SessionMessage, SessionSummary } from '../src/core/api/dto'
import { mergeServerPage, appendMessage } from '../src/core/chat/merge'
import { parseL1Receipt, parseSaveReceipt } from '../src/core/chat/receipt'
import { canRecall, recallLabel } from '../src/core/chat/recall'
import {
  buildSessionRow,
  buildSessionRows,
  filterSessionRows,
  groupSessionRows,
} from '../src/core/chat/sessionList'
import { dayDivider, dayKey, formatClock, smartTime } from '../src/core/chat/time'

const NOW = new Date(2026, 9, 23, 15, 0, 0).getTime()

const RAMA = { uid: 'rama-0001', name: 'Rama', kind: 'rama', source: 'manual' }
const IMPORTED = { uid: 'user-1001', name: '小珊', kind: 'char', source: 'qq' }

function summary(patch: Partial<SessionSummary> = {}): SessionSummary {
  return {
    id: '11111111-2222-3333-4444-555555555555',
    started_at: NOW - 60_000,
    ended_at: null,
    message_count: 12,
    unread: 0,
    persona_uid: 'rama-0001',
    channel: 'local',
    external_ref: null,
    ...patch,
  }
}

function message(patch: Partial<SessionMessage> = {}): SessionMessage {
  return {
    id: 'm-1',
    role: 'user',
    content: '你好',
    persona_uid: 'rama-0001',
    created_at: NOW - 1_000,
    is_proactive: false,
    sender_ref: null,
    sender_name: null,
    recalled_at: null,
    ...patch,
  }
}

describe('会话行', () => {
  it('产品人格标默认，副行回退消息数', () => {
    const row = buildSessionRow(summary(), RAMA, NOW)
    expect(row.tags.map((tag) => tag.label)).toEqual(['默认人格'])
    expect(row.preview).toBe('12 条消息')
    expect(row.readonly).toBe(false)
    expect(row.initial).toBe('R')
  })

  it('后端给出预览时优先展示预览', () => {
    const row = buildSessionRow(
      summary({ last_message_preview: '那就把它当成一次探险' }),
      RAMA,
      NOW,
    )
    expect(row.preview).toBe('那就把它当成一次探险')
  })

  it('导入人格的会话只读并带导入标签', () => {
    const row = buildSessionRow(
      summary({ persona_uid: 'user-1001', channel: 'local' }),
      IMPORTED,
      NOW,
    )
    expect(row.imported).toBe(true)
    expect(row.readonly).toBe(true)
    expect(row.tags.map((tag) => tag.label)).toEqual(['导入', '只读'])
  })

  it('社交通道会话标通道名并只读', () => {
    const row = buildSessionRow(
      summary({ persona_uid: 'user-1001', channel: 'qq' }),
      IMPORTED,
      NOW,
    )
    expect(row.tags.map((tag) => tag.label)).toEqual(['导入', 'QQ', '只读'])
  })

  it('未读数不为负', () => {
    const row = buildSessionRow(summary({ unread: -3 }), RAMA, NOW)
    expect(row.unread).toBe(0)
  })
})

describe('会话筛选与分组', () => {
  const rows = buildSessionRows(
    [
      summary({ id: 'aaaaaaaa-0000', started_at: NOW - 60_000, persona_uid: 'rama-0001' }),
      summary({
        id: 'bbbbbbbb-0000',
        started_at: NOW - 3 * 86_400_000,
        persona_uid: 'user-1001',
        message_count: 3,
      }),
    ],
    (uid) => (uid === 'user-1001' ? IMPORTED : RAMA),
    NOW,
  )

  it('按显示名与副行过滤', () => {
    expect(filterSessionRows(rows, 'Rama')).toHaveLength(1)
    expect(filterSessionRows(rows, '3 条消息')).toHaveLength(1)
    expect(filterSessionRows(rows, '')).toHaveLength(2)
    expect(filterSessionRows(rows, '不存在')).toHaveLength(0)
  })

  it('按今天与更早分组', () => {
    const groups = groupSessionRows(rows, NOW)
    expect(groups.today.map((row) => row.session.id)).toEqual(['aaaaaaaa-0000'])
    expect(groups.earlier.map((row) => row.session.id)).toEqual(['bbbbbbbb-0000'])
  })
})

describe('撤回判据', () => {
  it('窗口内的本人消息可撤回', () => {
    expect(canRecall(message(), { readonly: false, now: NOW })).toBe(true)
  })

  it('超窗、已撤回、助手消息与只读会话均不可撤回', () => {
    expect(canRecall(message({ created_at: NOW - 121_000 }), { readonly: false, now: NOW })).toBe(
      false,
    )
    expect(canRecall(message({ recalled_at: NOW }), { readonly: false, now: NOW })).toBe(false)
    expect(canRecall(message({ role: 'assistant' }), { readonly: false, now: NOW })).toBe(false)
    expect(canRecall(message(), { readonly: true, now: NOW })).toBe(false)
  })

  it('系统条文案区分本人与对方', () => {
    expect(recallLabel(message(), 'Rama')).toBe('你撤回了一条消息')
    expect(recallLabel(message({ role: 'assistant', sender_name: '小珊' }), 'Rama')).toBe(
      '小珊撤回了一条消息',
    )
  })
})

describe('消息合并', () => {
  it('以服务端页首条为接缝保留更早消息', () => {
    const existing = [message({ id: 'a' }), message({ id: 'b' }), message({ id: 'local-1' })]
    const page = [message({ id: 'b' }), message({ id: 'c' })]
    expect(mergeServerPage(existing, page).map((item) => item.id)).toEqual(['a', 'b', 'c'])
  })

  it('页首不在本地列表时以页内首个命中消息为接缝', () => {
    const existing = [message({ id: 'a' }), message({ id: 'b' }), message({ id: 'c' })]
    const page = [message({ id: 'new-1' }), message({ id: 'c' })]
    expect(mergeServerPage(existing, page).map((item) => item.id)).toEqual([
      'a',
      'b',
      'new-1',
      'c',
    ])
  })

  it('找不到接缝时整页替换', () => {
    const existing = [message({ id: 'local-1' })]
    const page = [message({ id: 'server-1' }), message({ id: 'server-2' })]
    expect(mergeServerPage(existing, page).map((item) => item.id)).toEqual([
      'server-1',
      'server-2',
    ])
  })

  it('空页保持原列表，追加按标识去重', () => {
    const existing = [message({ id: 'a' })]
    expect(mergeServerPage(existing, [])).toBe(existing)
    expect(appendMessage(existing, message({ id: 'a' }))).toBe(existing)
    expect(appendMessage(existing, message({ id: 'b' }))).toHaveLength(2)
  })
})

describe('回执解析', () => {
  it('识别保存成功与无活跃会话', () => {
    expect(parseSaveReceipt('{"status":"ok","l1_generated":true}')?.l1_generated).toBe(true)
    expect(parseSaveReceipt('{"status":"no_active_session","l1_generated":false}')?.status).toBe(
      'no_active_session',
    )
  })

  it('非法 JSON 与未知状态返回 null', () => {
    expect(parseSaveReceipt('not-json')).toBeNull()
    expect(parseSaveReceipt('{"status":"weird","l1_generated":false}')).toBeNull()
    expect(parseL1Receipt('{"l1_generated":false,"reason":"no_messages"}')?.reason).toBe(
      'no_messages',
    )
    expect(parseL1Receipt('{"summary":"x"}')).toBeNull()
  })
})

describe('时间文案', () => {
  it('时钟与自然日键', () => {
    expect(formatClock(new Date(2026, 9, 23, 9, 5).getTime())).toBe('09:05')
    expect(dayKey(new Date(2026, 9, 23, 23, 0).getTime())).toBe('2026-10-23')
  })

  it('日期分隔条区分今天 / 昨天 / 更早', () => {
    expect(dayDivider(new Date(2026, 9, 23, 8, 0).getTime(), NOW)).toBe('今天')
    expect(dayDivider(new Date(2026, 9, 22, 8, 0).getTime(), NOW)).toBe('昨天')
    expect(dayDivider(new Date(2026, 9, 1, 8, 0).getTime(), NOW)).toBe('10月1日')
    expect(dayDivider(new Date(2025, 11, 31, 8, 0).getTime(), NOW)).toBe('2025年12月31日')
  })

  it('列表时间：刚刚 / 分钟 / 昨天 / 绝对日期', () => {
    expect(smartTime(NOW - 10_000, NOW)).toBe('刚刚')
    expect(smartTime(NOW - 5 * 60_000, NOW)).toBe('5分钟前')
    expect(smartTime(new Date(2026, 9, 22, 8, 0).getTime(), NOW)).toBe('昨天 08:00')
    expect(smartTime(new Date(2026, 9, 1, 8, 0).getTime(), NOW)).toBe('10月1日')
  })

  it('非法时间戳返回未知时间', () => {
    expect(formatClock(0)).toBe('未知时间')
    expect(smartTime(Number.NaN, NOW)).toBe('未知时间')
  })
})
