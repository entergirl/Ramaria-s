/**
 * tests/personaContent.test.ts — 人格视图的纯逻辑
 *
 * 覆盖:
 * - 类型标签、预设模式与拆解状态的中文映射（含未知枚举回退）
 * - 导入 / 导出统计文案的书写顺序与失败计数
 * - 编辑草稿的生成、脏判据、名称校验与字数截断
 */

import { describe, expect, it } from 'vitest'

import type { PersonaFull } from '../src/core/api/dto'
import { AVATAR_MAX_BYTES, avatarFileError } from '../src/core/persona/avatar'
import {
  clampText,
  draftChanged,
  draftOf,
  nameError,
  NAME_MAX,
} from '../src/core/persona/draft'
import {
  decomposeText,
  exportReceiptText,
  exportStatsText,
  importOutcomeText,
  personaKindLabel,
  personaKindTone,
  presetModeLabel,
  writeStatsText,
} from '../src/core/persona/format'

const NOW = Date.now()

function persona(partial: Partial<PersonaFull> = {}): PersonaFull {
  return {
    uid: 'char-0001',
    name: '小珊',
    kind: 'rama',
    source: 'manual',
    ref_id: null,
    avatar: null,
    config: null,
    description: '温和的陪伴型人格。',
    is_active: true,
    created_at: NOW - 1000,
    updated_at: NOW,
    ...partial,
  }
}

describe('类型与模式映射', () => {
  it('类型释义与色调按枚举给出，未知类型回退原值', () => {
    expect(personaKindLabel('rama')).toBe('本地人格')
    expect(personaKindLabel('user')).toBe('用户自身')
    expect(personaKindLabel('hist')).toBe('历史导入')
    expect(personaKindLabel('mystery')).toBe('mystery')
    expect(personaKindLabel('')).toBe('未知类型')

    expect(personaKindTone('rama')).toBe('brand')
    expect(personaKindTone('user')).toBe('accent')
    expect(personaKindTone('oc')).toBe('ok')
    expect(personaKindTone('mystery')).toBe('neutral')
  })

  it('导入模式文案含未知回退', () => {
    expect(presetModeLabel('create')).toBe('新建')
    expect(presetModeLabel('merge')).toBe('合并')
    expect(presetModeLabel('weird')).toBe('weird')
    expect(presetModeLabel('')).toBe('未知模式')
  })
})

describe('拆解与统计文案', () => {
  it('拆解未启用 / 已执行 / 跳过三态', () => {
    expect(
      decomposeText({ requested: false, executed: false, fact_count: 0, trait_count: 0 }),
    ).toBe('未启用')
    expect(
      decomposeText({ requested: true, executed: true, fact_count: 3, trait_count: 2 }),
    ).toBe('已执行（3 条事实 / 2 条特质）')
    expect(
      decomposeText({
        requested: true,
        executed: false,
        skipped_reason: 'privacy_unconfirmed',
        fact_count: 0,
        trait_count: 0,
      }),
    ).toBe('跳过：未确认隐私范围')
    expect(
      decomposeText({
        requested: true,
        executed: false,
        skipped_reason: 'llm_failed',
        fact_count: 0,
        trait_count: 0,
      }),
    ).toBe('跳过：对话模型不可用')
    expect(
      decomposeText({
        requested: true,
        executed: false,
        skipped_reason: 'other',
        fact_count: 0,
        trait_count: 0,
      }),
    ).toBe('跳过：other')
  })

  it('写入统计按「类别 数量」并列，失败数只在非零时出现', () => {
    expect(
      writeStatsText({ traits: 3, facts: 2, examples: 4, behavior_rules: 1, failed: 0 }),
    ).toBe('特质 3 / 事实 2 / 规则 1 / 示例 4')
    expect(
      writeStatsText({ traits: 0, facts: 0, examples: 0, behavior_rules: 0, failed: 2 }),
    ).toBe('特质 0 / 事实 0 / 规则 0 / 示例 0 / 失败 2')
  })

  it('导入结果提示含名称、uid 与模式', () => {
    const text = importOutcomeText({
      text: '',
      uid: 'char-0002',
      mode: 'merge',
      name: '雾',
      written: { traits: 1, facts: 2, examples: 0, behavior_rules: 0, failed: 0 },
      decompose: { requested: false, executed: false, fact_count: 0, trait_count: 0 },
    })
    expect(text).toContain('已导入「雾」')
    expect(text).toContain('char-0002')
    expect(text).toContain('合并')
    expect(text).toContain('特质 1')
  })

  it('导出统计与回执文案含提示词补丁与示例计数', () => {
    const stats = {
      traits: 3,
      facts: 2,
      behaviorRules: 1,
      blocks: 2,
      promptPatch: true,
      examplesIncluded: 4,
    }
    expect(exportStatsText(stats)).toBe(
      '特质 3 / 事实 2 / 规则 1 / 自由文本块 2 / 含提示词补丁 / 示例 4',
    )
    expect(
      exportStatsText({ ...stats, promptPatch: false, examplesIncluded: 0 }),
    ).toBe('特质 3 / 事实 2 / 规则 1 / 自由文本块 2 / 示例 0')
    expect(
      exportReceiptText({
        uid: 'char-0001',
        name: '小珊',
        writtenTo: 'C:/tmp/a.toml',
        stats,
      }),
    ).toContain('已导出到 C:/tmp/a.toml')
  })
})

describe('头像文件校验', () => {
  it('只放行图片类型，超限按体积拒绝', () => {
    expect(avatarFileError({ type: 'image/png', size: 1024 })).toBe('')
    expect(avatarFileError({ type: 'image/webp', size: AVATAR_MAX_BYTES })).toBe('')
    expect(avatarFileError({ type: 'text/plain', size: 1024 })).toBe(
      '请选择 PNG / JPEG / WebP / GIF 图片',
    )
    expect(avatarFileError({ type: 'image/heic', size: 1024 })).toBe(
      '请选择 PNG / JPEG / WebP / GIF 图片',
    )
    expect(avatarFileError({ type: 'image/png', size: AVATAR_MAX_BYTES + 1 })).toBe(
      '图片过大，请选择 20MB 以内的文件',
    )
  })
})

describe('编辑草稿', () => {
  it('缺失头像与描述按空串编辑', () => {
    expect(draftOf(persona({ avatar: null, description: null }))).toEqual({
      name: '小珊',
      avatar: '',
      description: '',
    })
  })

  it('脏判据逐字段比对，头像首尾空白不计差异', () => {
    const base = persona({ avatar: 'avatar.png', description: '旧描述' })
    expect(draftChanged(draftOf(base), base)).toBe(false)
    expect(
      draftChanged({ ...draftOf(base), avatar: '  avatar.png  ' }, base),
    ).toBe(false)
    expect(draftChanged({ ...draftOf(base), name: '小珊 ' }, base)).toBe(true)
    expect(draftChanged({ ...draftOf(base), avatar: '' }, base)).toBe(true)
    expect(draftChanged({ ...draftOf(base), description: '' }, base)).toBe(true)
  })

  it('名称校验与截断', () => {
    expect(nameError('  ')).toBe('名称不能为空')
    expect(nameError('小珊')).toBe('')
    expect(nameError('字'.repeat(NAME_MAX + 1))).toBe(`名称最多 ${NAME_MAX} 字`)
    expect(clampText('一二三四', 2)).toBe('一二')
    expect(clampText('一二', 4)).toBe('一二')
  })
})
