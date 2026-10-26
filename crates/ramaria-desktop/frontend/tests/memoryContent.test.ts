/**
 * tests/memoryContent.test.ts — 记忆页纯函数
 *
 * 覆盖:
 * - 人格分类与显示名（用户人格显示为「你」）
 * - 关键词拆分、效价 / 共享度 / 陈述方式映射
 * - L1 分组上下文解析的容错
 * - 知识分组的固定字段顺序与版本链排序
 */
import { describe, expect, it } from 'vitest'

import type { PersonaFact } from '../src/core/api/dto'
import { parseL1Context } from '../src/core/memory/context'
import {
  dayLabel,
  displayPersonaName,
  factSourceLabel,
  factTierLabel,
  orderFactGroups,
  personaCategory,
  personaInitial,
  presentationLabel,
  profileStatusLabel,
  shareLabel,
  signedDecimal,
  splitKeywords,
  valenceTone,
} from '../src/core/memory/format'

const NOW = Date.UTC(2026, 9, 24, 12, 0, 0)

function fact(id: number, field: string, createdAt: number, content = `事实 ${id}`): PersonaFact {
  return {
    id,
    field,
    content,
    status: 'active',
    tier: 'stable',
    confidence: 0.7,
    source: 'event',
    keyword_hint: null,
    version_of: null,
    created_at: createdAt,
  }
}

describe('记忆展示映射', () => {
  it('按类型与来源派生人格分类', () => {
    expect(personaCategory('rama', 'manual').label).toBe('默认人格')
    expect(personaCategory('user', 'system').label).toBe('用户人格')
    expect(personaCategory('char', 'qq').label).toBe('历史导入')
    expect(personaCategory('hist', 'local').label).toBe('历史导入')
    expect(personaCategory('oc', 'local').label).toBe('原创角色')
  })

  it('用户本人人格在参与者里显示为「你」', () => {
    const personas = [
      { uid: 'user-0001', name: '我', kind: 'user', source: 'system', is_active: true, created_at: 1 },
      { uid: 'rama-0001', name: '小珊', kind: 'rama', source: 'manual', is_active: true, created_at: 1 },
    ]
    expect(displayPersonaName('user-0001', personas)).toBe('你')
    expect(displayPersonaName('rama-0001', personas)).toBe('小珊')
    expect(displayPersonaName('char-9', personas)).toBe('char-9')
    expect(displayPersonaName(null, personas)).toBe('未知人格')
    expect(personaInitial(' 林霁 ')).toBe('林')
    expect(personaInitial('')).toBe('?')
  })

  it('关键词兼容中英文分隔符并去重', () => {
    expect(splitKeywords('咖啡馆, 独处、探店；独处')).toEqual(['咖啡馆', '独处', '探店'])
    expect(splitKeywords('')).toEqual([])
    expect(splitKeywords(null)).toEqual([])
  })

  it('数值与枚举映射带兜底', () => {
    expect(signedDecimal(0.42)).toBe('+0.42')
    expect(signedDecimal(-0.1)).toBe('-0.10')
    expect(signedDecimal(null)).toBe('—')
    expect(valenceTone(0.31)).toBe('pos')
    expect(valenceTone(-0.4)).toBe('neg')
    expect(valenceTone(0.2)).toBe('flat')
    expect(shareLabel(0.75)).toBe('高')
    expect(shareLabel(0.5)).toBe('中')
    expect(shareLabel(0.1)).toBe('低')
    expect(presentationLabel('subjective')).toBe('主观')
    expect(presentationLabel('unknown')).toBe('unknown')
    expect(factTierLabel('volatile')).toBe('动态')
    expect(factSourceLabel('l1')).toBe('会话')
    expect(profileStatusLabel('preliminary')).toBe('初步')
    expect(dayLabel(Date.UTC(2026, 9, 12), NOW)).toBe('10月12日')
    expect(dayLabel(Date.UTC(2025, 8, 1), NOW)).toBe('2025年9月1日')
  })
})

describe('L1 分组上下文', () => {
  it('解析参与者与消息数', () => {
    const context = parseL1Context(
      JSON.stringify({ chat_partners: ['user-0001', 'rama-0001', 'user-0001'], message_count: 8 }),
    )
    expect(context.partners).toEqual(['user-0001', 'rama-0001'])
    expect(context.messageCount).toBe(8)
  })

  it('坏数据降级为空结果', () => {
    expect(parseL1Context('not-json')).toEqual({ partners: [], messageCount: null })
    expect(parseL1Context('[]')).toEqual({ partners: [], messageCount: null })
    expect(parseL1Context(JSON.stringify({ chat_partners: 'x', message_count: -3 }))).toEqual({
      partners: [],
      messageCount: null,
    })
    expect(parseL1Context(null)).toEqual({ partners: [], messageCount: null })
  })
})

describe('知识分组排序', () => {
  it('按固定字段顺序排列，组内按时间倒序', () => {
    const groups = orderFactGroups({
      兴趣爱好: [fact(1, '兴趣爱好', 100), fact(2, '兴趣爱好', 300)],
      基础信息: [fact(3, '基础信息', 200)],
      未知字段: [fact(4, '未知字段', 400)],
    })
    expect(groups.map((group) => group.field)).toEqual(['基础信息', '兴趣爱好', '未知字段'])
    expect(groups[1]?.facts.map((item) => item.id)).toEqual([2, 1])
  })
})
