/**
 * tests/debugCore.test.ts — 调试视图纯函数的换算与解析
 *
 * 覆盖:
 * - 关键词三态分组（空组保留、未知状态归入待确认）
 * - 说话风格统计解析为五维分组、每千字频率与状态色调
 * - 统计参数损坏时的非解析标记
 * - 评测报告摘要：类型识别、条件得分、结论与辅助指标
 * - 产物文件的大小与修改时间文案
 */

import { describe, expect, it } from 'vitest'

import type { EvalFile, KeywordEntry, StyleStats } from '../src/core/api/dto'
import {
  buildStyleModel,
  evalFileMeta,
  fileSizeLabel,
  groupKeywords,
  parseStyleStats,
  ratioLabel,
  scoreLabel,
} from '../src/core/debug/format'
import { detectKind, summarizeArtifact } from '../src/core/debug/eval'

function entry(keyword: string, status: string, useCount = 1): KeywordEntry {
  return {
    keyword,
    use_count: useCount,
    status,
    canonical_id: status === 'canonical' ? null : 1,
    canonical_keyword: status === 'canonical' ? null : 'rama',
    created_at: 1_700_000_000_000,
  }
}

function statsJson(): string {
  return JSON.stringify({
    sample_count: 312,
    total_chars: 5200,
    total_sentences: 260,
    word_freq: [
      ['呢', 40],
      ['呀', 12],
    ],
    sentence_len_mean: 18.4,
    sentence_len_p25: 11,
    sentence_len_p75: 26,
    slash_count: 4,
    comma_count: 120,
    newline_count: 30,
    exclaim_count: 8,
    question_count: 22,
    ellipsis_count: 18,
    paren_count: 3,
    tilde_count: 5,
    sentiment_mean: 0.32,
    sentiment_std: 0.4,
    sentiment_n: 310,
    interjection_count: 52,
    sentiment_word_messages: 240,
    topic_freq: [['工作', 9]],
  })
}

function styleStats(raw = statsJson()): StyleStats {
  return {
    persona_uid: 'rama-0001',
    sample_count: 312,
    baseline_version: 2,
    status: 'ready',
    status_label: '充分',
    rule_source: 'template',
    rule_source_label: '模板',
    rule_text: '  多用短句，句尾常带语气词。  ',
    stats_json: raw,
    updated_at: 1_760_000_000_000,
  }
}

/** 评测报告的裁剪夹具：只留摘要需要的字段 */
function evalReportFixture() {
  return {
    artifact_kind: 'ramaria_eval_report',
    schema_version: 1,
    run_id: 'm9-c2',
    source: {
      items_sha256: 'abc',
      ruleset_version: '2026-11-01.2',
      score_schema_version: 3,
      judge: { status: 'configured', model: 'judge-x', votes: 3 },
    },
    runs: [],
    conditions: [
      {
        condition_id: 'u1-y1/flat-rag/800/0',
        tier: 'u1-y1',
        pipeline: 'flat-rag',
        budget_chars: 800,
        rubric: { micro_mean_ratio: 0.16316666666666668, micro_full_hit_rate: 0.15333333333333332 },
        retrieval: {
          message: { coverage_post: 0.59 },
          artifact: { coverage_post: 0.5764221181037774 },
        },
      },
      {
        condition_id: 'u1-y1/layered/800/0',
        pipeline: 'layered',
        budget_chars: 800,
        rubric: { micro_mean_ratio: 0.209 },
        retrieval: { artifact: {} },
      },
    ],
    comparisons: [
      { id: 'u1-y1/layered-vs-flat-rag', significant: true },
      { id: 'u5-y1/layered-vs-flat-rag', significant: false },
    ],
    hypotheses: [
      { id: 'H1', status: 'supported', summary: 'layered 得分率随规模严格递减', evidence: ['均值 0.209'] },
      { id: 'H2', status: 'not_evidenced', summary: '预算轴数据不足' },
    ],
    missing: [{ scope: 'report', kind: 'not_run', detail: '预算轴缺少条件' }],
  }
}

describe('调试纯函数', () => {
  it('关键词按三态分组，空组保留计数位', () => {
    const groups = groupKeywords([
      entry('珊瑚菌', 'canonical', 46),
      entry('小 rama', 'alias', 18),
      entry('未知态词', 'weird', 2),
    ])

    expect(groups.map((group) => group.status)).toEqual(['canonical', 'alias', 'pending'])
    expect(groups[0]?.items).toHaveLength(1)
    expect(groups[1]?.items[0]?.keyword).toBe('小 rama')
    // 未知状态不丢词条，归入待确认组
    expect(groups[2]?.items.map((item) => item.keyword)).toEqual(['未知态词'])
    // 默认只展开规范词
    expect(groups.map((group) => group.openByDefault)).toEqual([true, false, false])
  })

  it('风格统计解析为五维分组并换算每千字频率', () => {
    const parsed = parseStyleStats(statsJson())

    expect(parsed.parsable).toBe(true)
    expect(parsed.groups.map((group) => group.id)).toEqual([
      'words',
      'sentence',
      'punct',
      'emotion',
      'topics',
    ])

    const sentence = parsed.groups.find((group) => group.id === 'sentence')
    const meanField = sentence?.fields.find((field) => field.label === '平均句长')
    expect(meanField?.value).toBe('18.4 字')

    const punct = parsed.groups.find((group) => group.id === 'punct')
    const comma = punct?.fields.find((field) => field.label === '逗号')
    expect(comma?.value).toBe('120 次 · 23.1‰')

    const emotion = parsed.groups.find((group) => group.id === 'emotion')
    const hitRate = emotion?.fields.find((field) => field.label === '情绪词命中率')
    expect(hitRate?.value).toBe('76.9%')

    const words = parsed.groups.find((group) => group.id === 'words')
    expect(words?.chips[0]).toEqual({ text: '呢', count: 40 })
  })

  it('统计参数损坏时只给非解析标记', () => {
    const parsed = parseStyleStats('{不是 JSON')
    expect(parsed.parsable).toBe(false)
    expect(parsed.groups).toEqual([])

    const model = buildStyleModel(styleStats('null'))
    expect(model.parsable).toBe(false)
    expect(model.ruleText).toBe('多用短句，句尾常带语气词。')
    expect(model.statusTone).toBe('ok')
    expect(model.sampleCount).toBe(312)
  })

  it('评测报告按条件行展开并带结论', () => {
    const report = evalReportFixture()

    expect(detectKind(report)).toBe('eval_report')
    // 只有产物标识、没有数字 schema_version 时不认
    expect(detectKind({ artifact_kind: 'ramaria_eval_report' })).toBe('unknown')

    const summary = summarizeArtifact(report)
    expect(summary.kind).toBe('eval_report')
    expect(summary.meta.judgeUsed).toBe(true)
    expect(summary.variants).toHaveLength(2)
    expect(summary.variants[0]?.name).toBe('u1-y1/flat-rag/800/0')
    expect(summary.variants[0]?.gateText).toBe('flat-rag / 800 字符')
    expect(summary.variants[0]?.scores.map((item) => item.label)).toEqual([
      '得分率',
      '覆盖(后)',
      '全要点命中',
    ])
    expect(summary.variants[0]?.scores[1]?.value).toBeCloseTo(0.576)
    // 缺值不塞 0：第二个条件只保留得分率
    expect(summary.variants[1]?.scores).toEqual([{ label: '得分率', value: 0.209 }])

    expect(summary.auxiliary).toEqual([
      { label: '条件数', value: 2, unit: 'count' },
      { label: '显著比较数', value: 1, unit: 'count' },
      { label: '缺项数', value: 1, unit: 'count' },
    ])
    expect(summary.hypotheses?.map((item) => item.id)).toEqual(['H1', 'H2'])
    expect(summary.hypotheses?.[0]).toEqual({
      id: 'H1',
      status: 'supported',
      summary: 'layered 得分率随规模严格递减',
    })
    expect(summary.hasContent).toBe(true)
  })

  it('评测报告缺字段时降级为空态', () => {
    const summary = summarizeArtifact({ artifact_kind: 'ramaria_eval_report', schema_version: 1 })
    expect(summary.kind).toBe('eval_report')
    expect(summary.variants).toEqual([])
    expect(summary.hypotheses).toBeNull()
    expect(summary.meta.judgeUsed).toBeNull()
    expect(summary.auxiliary?.map((item) => item.value)).toEqual([0, 0, 0])
    expect(summary.hasContent).toBe(false)

    const skipped = summarizeArtifact({
      artifact_kind: 'ramaria_eval_report',
      schema_version: 1,
      source: { judge: { status: 'skipped' } },
      hypotheses: [{ id: 'H1', status: 'partial', summary: '仅条件成立' }],
    })
    expect(skipped.meta.judgeUsed).toBe(false)
    // 条件为空但有结论，仍算有内容
    expect(skipped.hasContent).toBe(true)
  })
  it('无法识别的产物安全降级', () => {
    const empty = summarizeArtifact(null)
    expect(empty.kind).toBe('unknown')
    expect(empty.variants).toEqual([])
    expect(empty.auxiliary).toBeNull()
    expect(empty.hypotheses).toBeNull()
    expect(empty.hasContent).toBe(false)

    // 旧版探针产物不再识别：有结构化字段也给空摘要
    const legacy = summarizeArtifact({
      dataset_seed: 42,
      generated_at: 't',
      variants: [{ variant_id: 'B1', fact_score: 0.8 }],
    })
    expect(legacy.kind).toBe('unknown')
    expect(legacy.variants).toEqual([])
    expect(legacy.hasContent).toBe(false)
  })

  it('文件大小与修改时间按档位展示', () => {
    expect(fileSizeLabel(512)).toBe('512 B')
    expect(fileSizeLabel(2048)).toBe('2.0 KB')
    expect(fileSizeLabel(3 * 1024 * 1024)).toBe('3.0 MB')

    const file: EvalFile = {
      name: 'report.json',
      path: 'C:/eval/report.json',
      size: 1024,
      modified_at: Date.now(),
    }
    expect(evalFileMeta(file)).toContain('1.0 KB · ')
    expect(scoreLabel(0.4444)).toBe('0.444')
    expect(ratioLabel(0.1234)).toBe('12.3%')
    expect(ratioLabel(Number.NaN)).toBe('—')
  })
})
