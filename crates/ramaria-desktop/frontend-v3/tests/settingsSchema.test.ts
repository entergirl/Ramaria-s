/**
 * tests/settingsSchema.test.ts — 设置键面单源校验
 *
 * 覆盖:
 * - 界面键与 `config/default.toml` 键面（机器基线）逐键一致，无多余与遗漏
 * - 字段 / 组标识唯一，路径与键一致
 * - 控件选项与数值约束的声明完整性
 */

import { describe, expect, it } from 'vitest'

import baseline from '../../../../scripts/config-baseline-3.0.json'
import { INTERNAL_KEYS, SETTINGS_FIELDS, SETTINGS_GROUPS, schemaKeys } from '../src/config/settingsSchema'
import { ADVANCED_SECTIONS, BASIC_SECTIONS } from '../src/config/settingsSections'

function baselineKeys(): string[] {
  return baseline.keys
}

describe('设置键面', () => {
  it('界面键 = 配置基线键 − 版本字段 − 文件路径', () => {
    const internal = new Set<string>(INTERNAL_KEYS)
    const expected = baselineKeys()
      .filter((key) => !internal.has(key))
      .sort()
    expect(schemaKeys().slice().sort()).toEqual(expected)
  })

  it('键唯一且与路径一致', () => {
    const keys = schemaKeys()
    expect(new Set(keys).size).toBe(keys.length)
    for (const field of SETTINGS_FIELDS) {
      expect(field.key).toBe(field.path.join('.'))
      expect(field.label.trim().length).toBeGreaterThan(0)
    }
  })

  it('组标识唯一，且每张卡都有字段', () => {
    const ids = SETTINGS_GROUPS.map((group) => group.id)
    expect(new Set(ids).size).toBe(ids.length)
    for (const group of SETTINGS_GROUPS) {
      expect(group.fields.length).toBeGreaterThan(0)
    }
  })

  it('选择类控件都带选项，数值约束为有限数', () => {
    for (const field of SETTINGS_FIELDS) {
      if (field.control === 'select' || field.control === 'whitelist' || field.control === 'order') {
        expect(field.options?.length, `${field.key} 缺少选项`).toBeGreaterThan(0)
      }
      for (const bound of [field.min, field.max, field.step]) {
        if (bound !== undefined) {
          expect(Number.isFinite(bound), `${field.key} 约束非法`).toBe(true)
        }
      }
    }
  })

  it('小节引用的字段都在本组内', () => {
    for (const group of SETTINGS_GROUPS) {
      const keys = new Set(group.fields.map((field) => field.key))
      for (const block of group.blocks ?? []) {
        for (const key of block.keys) {
          expect(keys.has(key), `${group.id} 的小节引用了不存在的字段 ${key}`).toBe(true)
        }
      }
    }
  })
})

describe('设置目录', () => {
  it('基础 5 子页、进阶 8 类；卡片 id 唯一且指向已知组', () => {
    expect(BASIC_SECTIONS).toHaveLength(5)
    expect(ADVANCED_SECTIONS).toHaveLength(8)

    const groupIds = new Set(SETTINGS_GROUPS.map((group) => group.id))
    const cardIds: string[] = []
    for (const section of [...BASIC_SECTIONS, ...ADVANCED_SECTIONS]) {
      for (const card of section.cards) {
        cardIds.push(card.id)
        if (['schema', 'backend', 'embedding', 'channel', 'mcp'].includes(card.kind)) {
          expect(groupIds.has(card.id), `${card.id} 未在键表中声明`).toBe(true)
        }
      }
    }
    expect(new Set(cardIds).size).toBe(cardIds.length)
  })

  it('进阶 21 组全部出现在目录中', () => {
    const advancedCards = ADVANCED_SECTIONS.flatMap((section) => section.cards)
    expect(advancedCards).toHaveLength(21)
  })
})
