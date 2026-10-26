/**
 * tests/rulesContent.test.ts — 规则展示映射与编辑草稿判定
 *
 * 覆盖:
 * - 来源标签 / 色调、启停标签、候选规则判定与占位文案
 * - 情境关键词去重与截断、证据关键词拆分
 * - 禁忌词输入归一化（中文逗号 / 顿号）与「是否有改动」
 * - 规则文本校验：已有文本不允许清空，候选规则可留空
 */
import { describe, expect, it } from 'vitest'

import type { BehaviorRuleView, RuleEvidenceItem } from '../src/core/api/dto'
import {
  avoidText,
  CANDIDATE_TEXT,
  evidenceKeywords,
  isCandidateRule,
  normalizeAvoid,
  parseAvoidInput,
  reactionError,
  ruleEditChanged,
  ruleKeywords,
  ruleSourceLabel,
  ruleSourceTone,
  ruleStatusLabel,
  ruleText,
} from '../src/core/rules/format'

function rule(partial: Partial<BehaviorRuleView> = {}): BehaviorRuleView {
  return {
    id: 7,
    persona_uid: 'rama-0001',
    situation: {
      keywords: [],
      centroid: null,
      response_centroid: null,
      valence_mean: 0,
      valence_std: 0,
      sample_count: 0,
      presentation_dist: [],
      situation_strength_mean: 3,
      time_span_days: 0,
      trait_refs: [],
    },
    reaction: '深夜不要主动发起高强度话题。',
    params: {
      emotional_intensity: 0.42,
      proactiveness: 0.5,
      detail_level: 0.5,
      formality: 0.5,
    },
    avoid: ['说教'],
    evidence: [],
    confidence: 0.91,
    stability: 0.8,
    source: 'manual',
    enabled: true,
    created_at: 1,
    updated_at: 2,
    ...partial,
  }
}

describe('规则展示映射', () => {
  it('来源标签与色调分开表达', () => {
    expect(ruleSourceLabel('manual')).toBe('手工')
    expect(ruleSourceLabel('auto')).toBe('自动')
    expect(ruleSourceTone('manual')).toBe('accent')
    expect(ruleSourceTone('auto')).toBe('ok')
  })

  it('启停标签以文字表意', () => {
    expect(ruleStatusLabel(true)).toBe('已启用')
    expect(ruleStatusLabel(false)).toBe('已停用')
  })

  it('候选规则判定同时兼容 null 与空白文本', () => {
    expect(isCandidateRule(rule({ reaction: null }))).toBe(true)
    expect(isCandidateRule(rule({ reaction: '   ' }))).toBe(true)
    expect(isCandidateRule(rule())).toBe(false)
  })

  it('候选规则用固定占位文案，不出现空白正文', () => {
    expect(ruleText(rule({ reaction: null }))).toBe(CANDIDATE_TEXT)
    expect(ruleText(rule({ reaction: '  深夜别聊重活  ' }))).toBe('深夜别聊重活')
  })

  it('情境关键词去空去重并按上限截断', () => {
    const words = [' 夜间 ', '夜间', '', '边界', '工作', '倾诉']
    expect(ruleKeywords(rule({ situation: { ...rule().situation, keywords: words } }))).toEqual([
      '夜间',
      '边界',
      '工作',
      '倾诉',
    ])
    expect(
      ruleKeywords(
        rule({
          situation: { ...rule().situation, keywords: ['a', 'b', 'c', 'd'] },
        }),
        2,
      ),
    ).toEqual(['a', 'b'])
  })

  it('证据关键词兼容逗号串与空值', () => {
    const item: RuleEvidenceItem = {
      event_id: 1,
      weight: 0.5,
      title: '标题',
      summary: '摘要',
      paraphrase: null,
      keywords: '夜间, 边界，倾诉',
    }
    expect(evidenceKeywords(item)).toEqual(['夜间', '边界', '倾诉'])
    expect(evidenceKeywords({ ...item, keywords: null })).toEqual([])
  })
})

describe('编辑草稿', () => {
  it('禁忌词归一化兼容中文逗号与顿号', () => {
    expect(parseAvoidInput('说教，训诫、命令')).toEqual(['说教', '训诫', '命令'])
    expect(parseAvoidInput('  ')).toEqual([])
    expect(avoidText(['说教', '训诫'])).toBe('说教,训诫')
  })

  it('禁忌词展示归一：去空去重且不限条数', () => {
    expect(normalizeAvoid([' 说教 ', '说教', '', '训诫'])).toEqual(['说教', '训诫'])
    expect(normalizeAvoid([])).toEqual([])
  })

  it('文本或禁忌词任一变化即视为有改动', () => {
    const base = rule({ avoid: ['说教'] })
    expect(ruleEditChanged(base, '深夜不要主动发起高强度话题。', '说教')).toBe(false)
    expect(ruleEditChanged(base, ' 深夜不要主动发起高强度话题。 ', '说教')).toBe(false)
    expect(ruleEditChanged(base, '换一句', '说教')).toBe(true)
    expect(ruleEditChanged(base, '深夜不要主动发起高强度话题。', '说教，训诫')).toBe(true)
  })

  it('已有文本的规则不允许清空，候选规则可留空', () => {
    expect(reactionError(rule(), '   ')).toBe('规则文本不能为空')
    expect(reactionError(rule(), '新的文本')).toBe('')
    expect(reactionError(rule({ reaction: null }), '')).toBe('')
  })
})
