/**
 * tests/debugCore.test.ts — 调试视图纯函数的换算与解析
 *
 * 覆盖:
 * - 关键词三态分组（空组保留、未知状态归入待确认）
 * - 说话风格统计解析为五维分组、每千字频率与状态色调
 * - 统计参数损坏时的非解析标记
 * - probe 产物摘要：类型识别、档位得分、逐轮聚合与辅助指标
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
import { gateDescription, summarizeArtifact } from '../src/core/debug/probe'

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

  it('产物摘要识别类型、档位得分与辅助指标', () => {
    const artifact = summarizeArtifact({
      persona_uid: 'rama-0001',
      judge_used: true,
      embedding_used: false,
      generated_at: '2026-09-01T00:00:00Z',
      variants: [
        { variant_id: 'B1', fact_score: 0.8, tone_score: 3.2, params: { ablation: 'B1' } },
        { variant_id: 'F0', fact_score_norm: 0.52, params: { ablation: 'F0' } },
      ],
      auxiliary: {
        evidence_traceability_rate: 0.9,
        profile_regression_output_stability: 0.2,
        annotation: '不进指标',
      },
    })

    expect(artifact.kind).toBe('report')
    expect(artifact.meta.personaUid).toBe('rama-0001')
    expect(artifact.meta.judgeUsed).toBe(true)
    expect(artifact.variants).toHaveLength(2)
    expect(artifact.variants[0]?.gateText).toBe('完整基线（RAG + 四层）')
    expect(artifact.variants[0]?.scores).toEqual([
      { label: '事实', value: 0.8 },
      { label: '语气', value: 3.2 },
    ])
    expect(artifact.variants[1]?.scores).toEqual([{ label: '事实(归一)', value: 0.52 }])
    expect(artifact.auxiliary?.map((metric) => metric.label)).toEqual([
      '证据链可追溯率',
      '画像回归稳定性',
    ])
    expect(artifact.hasContent).toBe(true)
  })

  it('产物逐轮维度聚合兼容数组与对象两种形态', () => {
    const arrayForm = summarizeArtifact({
      judge_used: false,
      variants: [
        {
          variant_id: 'I_behavior',
          dimension_scores: [
            { dimension: 'fact', mean: 0.61, n: 3, ci95_low: 0.5, ci95_high: 0.72 },
          ],
        },
      ],
    })
    const score = arrayForm.variants[0]?.scores[0]
    expect(score?.label).toBe('事实（逐轮）')
    expect(score?.value).toBe(0.61)
    expect(score?.detail).toBe('n=3 CI[0.5, 0.72]')

    const objectForm = summarizeArtifact({
      judge_used: false,
      variants: {
        S_behavior: { tone_mean: 3.5, dimension_scores: { tone: { mean: 3.4, n: 2 } } },
      },
    })
    expect(objectForm.variants[0]?.name).toBe('S_behavior')
    expect(objectForm.variants[0]?.scores.map((item) => item.label)).toEqual([
      '语气均值',
      '语气（逐轮）',
    ])
  })

  it('畸形产物与未知档位安全降级', () => {
    const empty = summarizeArtifact(null)
    expect(empty.kind).toBe('unknown')
    expect(empty.variants).toEqual([])
    expect(empty.auxiliary).toBeNull()
    expect(empty.hasContent).toBe(false)

    const run = summarizeArtifact({ dataset_seed: 42, generated_at: 't', variants: [] })
    expect(run.kind).toBe('run')
    expect(gateDescription('unknown')).toBeNull()
    expect(gateDescription('I_behavior')).toBe('净增量：B1 基座 + behavior 专属层')
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
