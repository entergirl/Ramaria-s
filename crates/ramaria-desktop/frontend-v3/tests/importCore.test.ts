/**
 * tests/importCore.test.ts — 导入视图纯函数口径
 *
 * 覆盖:
 * - 表单默认值、路径取名、报告回填优先级、提交归一
 * - 阶段裁剪与格位状态、百分比 / 计数 / ETA（后端 EMA 与线性兜底）
 * - 成员排序与文案、报告 / 结果统计条目
 */

import { describe, expect, it } from 'vitest'

import type { ImportAnalysisReport, ImportProgressPayload, ImportResult } from '../src/core/api/dto'
import {
  applyReportDefaults,
  defaultImportForm,
  fileDisplayName,
  formError,
  submitPayload,
} from '../src/core/import/form'
import {
  continueMetrics,
  doneMetrics,
  formatEta,
  progressView,
  stageState,
  stagesForMode,
} from '../src/core/import/progress'
import {
  formatCount,
  memberSummary,
  memberText,
  modeLabel,
  reportMetrics,
  resultMetrics,
  timeRangeText,
} from '../src/core/import/report'

const T0 = 1_700_000_000_000

function report(partial: Partial<ImportAnalysisReport> = {}): ImportAnalysisReport {
  return {
    file_path: 'C:/exports/chat.json',
    self_id: 'u_self',
    self_name: '小明',
    self_uin: '10001',
    chat_name: '小珊',
    chat_type: 'private',
    members: [],
    other_name: '小珊',
    other_uid: 'u_peer',
    other_uin: null,
    time_range: '2024-01-01 ~ 2024-03-01',
    total_raw: 40,
    total_success: 38,
    total_degraded: 1,
    total_skipped: 1,
    session_count: 6,
    gap_minutes: 10,
    ...partial,
  }
}

function progress(partial: Partial<ImportProgressPayload> = {}): ImportProgressPayload {
  return {
    phase: 'l1',
    current: 5,
    total: 10,
    message: 'L1 5/10',
    ...partial,
  }
}

const RESULT: ImportResult = {
  success: true,
  mode: 'fast',
  report_summary: 'ok',
  sessions_written: 6,
  messages_written: 38,
  persona_uid: 'user-0001',
  persona_name: '小明',
  other_persona_uid: 'char-0001',
  other_persona_name: '小珊',
  self_name: '小明',
  chat_name: '小珊',
  chat_type: 'private',
  members: [],
  time_range: '2024-01-01 ~ 2024-03-01',
  skipped_count: 2,
}

describe('导入表单', () => {
  it('默认快速导入、双方、10 分钟切割，名称与 UID 留空', () => {
    const form = defaultImportForm()
    expect(form.mode).toBe('fast')
    expect(form.side).toBe('both')
    expect(form.gapMinutes).toBe(10)
    expect(form.selfName).toBe('')
    expect(form.selfUid).toBe('')
  })

  it('文件名兼容 Windows 与 POSIX 分隔符', () => {
    expect(fileDisplayName('D:\\qq\\chat.json')).toBe('chat.json')
    expect(fileDisplayName('/home/user/chat.json')).toBe('chat.json')
    expect(fileDisplayName('')).toBe('')
  })

  it('报告回填只填空值，用户已填名称不被覆盖', () => {
    const form = { ...defaultImportForm(), selfName: '我', otherName: '   ' }
    const next = applyReportDefaults(form, report())
    expect(next.selfName).toBe('我')
    expect(next.otherName).toBe('小珊')
  })

  it('提交归一：空白转 null、切割间隔夹到 1~1440', () => {
    const form = {
      ...defaultImportForm(),
      selfName: ' 我 ',
      selfUid: '   ',
      gapMinutes: 5000,
    }
    const payload = submitPayload(form)
    expect(payload.personaName).toBe('我')
    expect(payload.selfPersonaUid).toBeNull()
    expect(payload.gapMinutes).toBe(1440)

    const low = submitPayload({ ...form, gapMinutes: -3 })
    expect(low.gapMinutes).toBe(1)
  })

  it('未选文件时给出提示', () => {
    expect(formError(defaultImportForm())).toBe('请先选择 QQ 聊天记录文件')
    expect(formError({ ...defaultImportForm(), filePath: ' a.json ' })).toBe('')
  })
})

describe('进度口径', () => {
  it('快速导入只展示图片理解与会话摘要', () => {
    expect(stagesForMode('fast').map((item) => item.key)).toEqual(['vision', 'l1'])
    expect(stagesForMode('deep').map((item) => item.key)).toEqual([
      'vision',
      'l1',
      'l2',
      'l3',
    ])
  })

  it('阶段格位：当前之前为已完成，之后待开始，done 全完成', () => {
    expect(stageState('l1', 'vision')).toBe('done')
    expect(stageState('l1', 'l1')).toBe('active')
    expect(stageState('l1', 'l3')).toBe('pending')
    expect(stageState('done', 'l3')).toBe('done')
  })

  it('已知总量给百分比与计数，未知总量给不定进度', () => {
    const known = progressView(progress(), T0, T0 + 5000)
    expect(known.percent).toBe(50)
    expect(known.counts).toBe('5 / 10')
    expect(known.finished).toBe(false)

    const unknown = progressView(progress({ total: 0, current: 0 }), T0, T0)
    expect(unknown.percent).toBeNull()
    expect(unknown.counts).toBe('')
  })

  it('ETA 优先取后端 EMA，缺失时按已用时线性估算', () => {
    const backend = progressView(progress({ eta_seconds: 120 }), T0, T0 + 1000)
    expect(backend.etaText).toBe('约 2 分钟')

    const linear = progressView(progress(), T0, T0 + 10_000)
    expect(linear.etaText).toBe('约 10 秒')

    const early = progressView(progress(), T0, T0 + 1000)
    expect(early.etaText).toBe('计算中…')
  })

  it('接近完成与完成态文案', () => {
    expect(progressView(progress({ current: 10 }), T0, T0 + 5000).etaText).toBe('即将完成')
    const done = progressView(progress({ phase: 'done', current: 10, total: 10 }), T0, T0)
    expect(done.finished).toBe(true)
    expect(done.etaText).toBe('')
  })

  it('时长格式化跨分钟与小时', () => {
    expect(formatEta(20)).toBe('约 20 秒')
    expect(formatEta(90)).toBe('约 2 分钟')
    expect(formatEta(3 * 3600 + 25 * 60)).toBe('约 3 小时 25 分钟')
    expect(formatEta(2 * 3600)).toBe('约 2 小时')
  })

  it('完成统计：L1 成功率与级联标记', () => {
    const metrics = doneMetrics({
      phase: 'done',
      current: 5,
      total: 6,
      message: '完成',
      l1_success: 4,
      l1_failed: 1,
      l2_triggered: true,
      l3_triggered: false,
    })
    expect(metrics.map((item) => item.value)).toEqual(['4 / 5', '已触发'])

    expect(doneMetrics(null)).toEqual([])
    expect(doneMetrics(progress())).toEqual([])
  })

  it('续跑统计只输出事件携带的字段', () => {
    const metrics = continueMetrics({
      phase: 'done',
      current: 0,
      total: 0,
      message: '完成',
      reset_count: 3,
      vision_done: 2,
      vision_failed: 1,
      l1_success: 5,
      l1_failed: 0,
    })
    expect(metrics.map((item) => item.key)).toEqual(['reset', 'vision', 'l1'])
    expect(metrics[1].value).toBe('2 / 3')
    expect(continueMetrics(progress())).toEqual([])
  })
})

describe('报告与结果文案', () => {
  it('成员按消息数降序、同数按名称升序，并给出剩余数', () => {
    const summary = memberSummary(
      [
        { uid: 'a', uin: null, name: '乙', message_count: 3 },
        { uid: 'b', uin: null, name: '甲', message_count: 3 },
        { uid: 'c', uin: null, name: '丙', message_count: 9 },
      ],
      2,
    )
    expect(summary.total).toBe(3)
    // 同数按中文排序（甲乙丙的拼音序：甲 < 乙）
    expect(summary.top.map((item) => item.name)).toEqual(['丙', '甲'])
    expect(summary.rest).toBe(1)
  })

  it('成员文案带千分位与总人数', () => {
    expect(
      memberText([{ uid: 'a', uin: null, name: '小珊', message_count: 1204 }], 5),
    ).toBe('小珊（1,204 条）')
    expect(
      memberText(
        [
          { uid: 'a', uin: null, name: '甲', message_count: 5 },
          { uid: 'b', uin: null, name: '乙', message_count: 3 },
        ],
        1,
      ),
    ).toBe('甲（5 条） … 等共 2 人')
  })

  it('千分位与空值兜底', () => {
    expect(formatCount(1234567)).toBe('1,234,567')
    expect(formatCount(Number.NaN)).toBe('0')
    expect(timeRangeText('  ')).toBe('未知')
    expect(modeLabel('deep')).toBe('深度导入')
  })

  it('报告统计 5 项，结果统计含跳过数', () => {
    expect(reportMetrics(report()).map((item) => item.key)).toEqual([
      'raw',
      'success',
      'degraded',
      'skipped',
      'sessions',
    ])
    expect(resultMetrics(RESULT).map((item) => item.key)).toEqual([
      'sessions',
      'messages',
      'mode',
      'skipped',
    ])
  })
})
