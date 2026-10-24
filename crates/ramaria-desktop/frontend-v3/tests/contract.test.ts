/**
 * tests/contract.test.ts — 契约形状守卫
 *
 * 覆盖:
 * - 关键 DTO 的字段集合与命名形状（防止字段被误删或改成 camelCase）
 * - 命令表域数与条数（与 Rust 注册表的机器校验在 check_contract.py）
 * - 状态文案键遵循 `{view}.{state}.{reason}` 至少两段
 */

import { describe, expect, it } from 'vitest'

import { COMMANDS, commandNames } from '../src/core/api/commands'
import type {
  ChatDonePayload,
  IndexStatusView,
  PrivacyStatus,
  SessionDetail,
  SessionMessage,
  SessionSummary,
} from '../src/core/api/dto'
import { stateCopy, stateCopyKeys } from '../src/core/stateCopy'

const sessionMessage = {
  id: '9213eae6-0021-4398-81a8-2f3947d23d49',
  role: 'assistant',
  content: '今天先不出门。',
  persona_uid: 'rama-0001',
  created_at: 1781501460000,
  is_proactive: false,
  sender_ref: null,
  sender_name: null,
  recalled_at: null,
} satisfies SessionMessage

const sessionSummary = {
  id: '2515eb8c-2370-492a-84db-11bad21ce67e',
  started_at: 1781500000000,
  ended_at: null,
  message_count: 42,
  unread: 3,
  persona_uid: null,
  channel: 'local',
  external_ref: null,
} satisfies SessionSummary

const sessionDetail = {
  id: '2515eb8c-2370-492a-84db-11bad21ce67e',
  started_at: 1781500000000,
  ended_at: null,
  persona_uid: 'rama-0001',
  channel: 'local',
  total_messages: 1,
  has_more: false,
  messages: [sessionMessage],
} satisfies SessionDetail

const indexStatus = {
  status: 'building',
  loaded: 12,
  total: 40,
  failed: 0,
  percent: 30,
  updated_at: 1781501460000,
} satisfies IndexStatusView

const chatDone = {
  request_id: '0b1c2d3e',
  backend_id: 'deepseek-chat',
  total_chars: 12,
  content: '好，那就先不出门。',
} satisfies ChatDonePayload

const privacyNeedsConfirm = {
  status: 'NeedsConfirmation',
  provider_name: 'DeepSeek',
  base_url: 'https://api.deepseek.com',
} satisfies PrivacyStatus

describe('DTO 形状', () => {
  it('SessionMessage 为 9 字段且保持 snake_case', () => {
    expect(Object.keys(sessionMessage).sort()).toEqual(
      [
        'id',
        'role',
        'content',
        'persona_uid',
        'created_at',
        'is_proactive',
        'sender_ref',
        'sender_name',
        'recalled_at',
      ].sort(),
    )
  })

  it('SessionSummary 为 8 字段且保持 snake_case', () => {
    expect(Object.keys(sessionSummary).sort()).toEqual(
      [
        'id',
        'started_at',
        'ended_at',
        'message_count',
        'unread',
        'persona_uid',
        'channel',
        'external_ref',
      ].sort(),
    )
  })

  it('SessionDetail 为 8 字段且消息嵌套形状一致', () => {
    expect(Object.keys(sessionDetail).sort()).toEqual(
      [
        'id',
        'started_at',
        'ended_at',
        'persona_uid',
        'channel',
        'total_messages',
        'has_more',
        'messages',
      ].sort(),
    )
    expect(Object.keys(sessionDetail.messages[0])).toHaveLength(9)
  })

  it('IndexStatusView 为 8 字段，可选字段缺省不出现', () => {
    expect(Object.keys(indexStatus).sort()).toEqual(
      ['status', 'loaded', 'total', 'failed', 'percent', 'updated_at'].sort(),
    )
  })

  it('ChatDonePayload 用 request_id 关联请求', () => {
    expect(chatDone.request_id).toBe('0b1c2d3e')
    expect(Object.keys(chatDone).sort()).toEqual(
      ['request_id', 'backend_id', 'total_chars', 'content'].sort(),
    )
  })

  it('隐私状态以 PascalCase 变体名区分', () => {
    expect(privacyNeedsConfirm.status).toBe('NeedsConfirmation')
  })
})

describe('命令表', () => {
  it('覆盖 19 个域、78 条命令', () => {
    expect(Object.keys(COMMANDS)).toHaveLength(19)
    expect(commandNames()).toHaveLength(78)
  })

  it('命令名唯一', () => {
    const names = commandNames()
    expect(new Set(names).size).toBe(names.length)
  })
})

describe('状态文案', () => {
  it('键均为至少两段的点分形式', () => {
    const keys = stateCopyKeys()
    expect(keys.length).toBeGreaterThan(0)
    for (const key of keys) {
      expect(key.split('.').length).toBeGreaterThanOrEqual(2)
    }
  })

  it('未登记的键返回兜底文案而非空对象', () => {
    const fallback = stateCopy('unknown.view.state')
    expect(fallback.title.length).toBeGreaterThan(0)
    expect(fallback.description.length).toBeGreaterThan(0)
  })
})
