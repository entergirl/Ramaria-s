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
  ImportAnalysisReport,
  ImportProgressPayload,
  ImportResult,
  ImportStatusView,
  IndexStatusView,
  PersonaExportReceipt,
  PresetImportOutcome,
  PresetImportPreview,
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
  last_message_preview: '那就把它当成一次探险',
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

const presetPreview = {
  text: '新建 char-0002：特质 2 / 事实 1',
  uid: null,
  mode: 'create',
  name: '雾',
  decompose_enabled: true,
  privacy_confirmed: false,
} satisfies PresetImportPreview

const presetOutcome = {
  text: '写入结果：特质 2 / 事实 1',
  uid: 'char-0002',
  mode: 'create',
  name: '雾',
  written: { traits: 2, facts: 1, examples: 0, behavior_rules: 0, failed: 0 },
  decompose: { requested: true, executed: true, fact_count: 1, trait_count: 2 },
} satisfies PresetImportOutcome

const importReport = {
  file_path: 'C:/tmp/export.json',
  self_id: 'u_self',
  self_name: '小明',
  self_uin: '10001',
  chat_name: '小红',
  chat_type: 'private',
  members: [{ uid: 'u_peer', uin: null, name: '小红', message_count: 12 }],
  other_name: '小红',
  other_uid: 'u_peer',
  other_uin: null,
  time_range: '2023-01-01 ~ 2024-06-30',
  total_raw: 40,
  total_success: 38,
  total_degraded: 1,
  total_skipped: 1,
  session_count: 6,
  gap_minutes: 10,
} satisfies ImportAnalysisReport

const importResult = {
  success: true,
  mode: 'fast',
  report_summary: '解析 40 条，写入 38 条',
  sessions_written: 6,
  messages_written: 38,
  persona_uid: 'user-0001',
  persona_name: '小明',
  other_persona_uid: 'char-0001',
  other_persona_name: '小红',
  self_name: '小明',
  chat_name: '小红',
  chat_type: 'private',
  members: [],
  time_range: '2023-01-01 ~ 2024-06-30',
  skipped_count: 1,
} satisfies ImportResult

const importStatus = {
  post_active: false,
  pending_attachments: 2,
  skipped_attachments: 1,
  missing_l1_sessions: 3,
  resumable: true,
} satisfies ImportStatusView

const importDone = {
  phase: 'done',
  current: 5,
  total: 6,
  message: '导入完成',
  l1_success: 4,
  l1_failed: 1,
  l2_triggered: true,
  l3_triggered: true,
} satisfies ImportProgressPayload

const personaExport = {
  uid: 'char-0001',
  name: '小珊',
  writtenTo: 'C:/tmp/ramaria-persona-char-0001.toml',
  stats: {
    traits: 3,
    facts: 2,
    behaviorRules: 1,
    blocks: 2,
    promptPatch: true,
    examplesIncluded: 4,
  },
} satisfies PersonaExportReceipt

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

  it('SessionSummary 为 9 字段且保持 snake_case', () => {
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
        'last_message_preview',
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

  it('预设预览 6 字段，uid 可为 null', () => {
    expect(Object.keys(presetPreview).sort()).toEqual(
      ['text', 'uid', 'mode', 'name', 'decompose_enabled', 'privacy_confirmed'].sort(),
    )
    expect(presetPreview.uid).toBeNull()
  })

  it('预设导入结果为 6 字段，写入统计与拆解保持 snake_case', () => {
    expect(Object.keys(presetOutcome).sort()).toEqual(
      ['text', 'uid', 'mode', 'name', 'written', 'decompose'].sort(),
    )
    expect(Object.keys(presetOutcome.written).sort()).toEqual(
      ['traits', 'facts', 'examples', 'behavior_rules', 'failed'].sort(),
    )
    expect(Object.keys(presetOutcome.decompose).sort()).toEqual(
      ['requested', 'executed', 'fact_count', 'trait_count'].sort(),
    )
  })

  it('人格导出回执为 camelCase，hints 缺省省略', () => {
    expect(Object.keys(personaExport).sort()).toEqual(
      ['uid', 'name', 'writtenTo', 'stats'].sort(),
    )
    expect(Object.keys(personaExport.stats).sort()).toEqual(
      ['traits', 'facts', 'behaviorRules', 'blocks', 'promptPatch', 'examplesIncluded'].sort(),
    )
    expect('hints' in personaExport).toBe(false)
  })

  it('导入解析报告 17 字段，成员统计保持 snake_case', () => {
    expect(Object.keys(importReport).sort()).toEqual(
      [
        'file_path',
        'self_id',
        'self_name',
        'self_uin',
        'chat_name',
        'chat_type',
        'members',
        'other_name',
        'other_uid',
        'other_uin',
        'time_range',
        'total_raw',
        'total_success',
        'total_degraded',
        'total_skipped',
        'session_count',
        'gap_minutes',
      ].sort(),
    )
    expect(Object.keys(importReport.members[0]).sort()).toEqual(
      ['uid', 'uin', 'name', 'message_count'].sort(),
    )
  })

  it('导入结果 15 字段，session_ids 缺省不输出', () => {
    expect(Object.keys(importResult).sort()).toEqual(
      [
        'success',
        'mode',
        'report_summary',
        'sessions_written',
        'messages_written',
        'persona_uid',
        'persona_name',
        'other_persona_uid',
        'other_persona_name',
        'self_name',
        'chat_name',
        'chat_type',
        'members',
        'time_range',
        'skipped_count',
      ].sort(),
    )
    expect('session_ids' in importResult).toBe(false)
  })

  it('导入续跑状态 5 字段，resumable 由后端计算', () => {
    expect(Object.keys(importStatus).sort()).toEqual(
      [
        'post_active',
        'pending_attachments',
        'skipped_attachments',
        'missing_l1_sessions',
        'resumable',
      ].sort(),
    )
    expect(importStatus.resumable).toBe(true)
  })

  it('导入进度 done 负载可选统计字段按阶段输出', () => {
    expect(Object.keys(importDone).sort()).toEqual(
      [
        'phase',
        'current',
        'total',
        'message',
        'l1_success',
        'l1_failed',
        'l2_triggered',
        'l3_triggered',
      ].sort(),
    )
  })
})

describe('命令表', () => {
  it('覆盖 19 个域、82 条命令', () => {
    expect(Object.keys(COMMANDS)).toHaveLength(19)
    expect(commandNames()).toHaveLength(82)
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
