/**
 * tests/galleryCatalog.test.ts — 状态画廊的格位覆盖度
 *
 * 覆盖:
 * - 组件 15 行 86 格、视图 9 行 60 格，且逐行格数与设计矩阵一致
 * - 格位标识唯一、带行前缀，状态名与验收编号齐备
 * - 视图行与契约视图同集合
 * - 缩略文案：已登记视图读状态文案表，未登记视图给载体级说明
 */

import { describe, expect, it } from 'vitest'

import {
  COMPONENT_CELL_COUNT,
  COMPONENT_ROWS,
  VIEW_CELL_COUNT,
  VIEW_ROWS,
  allCellIds,
} from '../src/views/dev/gallery/catalog'
import { viewThumbCopy, viewThumbSpec } from '../src/views/dev/gallery/viewCells'

describe('组件格位', () => {
  it('15 行 86 格，行号连续', () => {
    expect(COMPONENT_ROWS).toHaveLength(15)
    expect(COMPONENT_CELL_COUNT).toBe(86)
    expect(COMPONENT_ROWS.map((row) => row.no)).toEqual(
      Array.from({ length: 15 }, (_, index) => index + 1),
    )
  })

  it('逐行格数与组件矩阵一致', () => {
    const counts = Object.fromEntries(COMPONENT_ROWS.map((row) => [row.key, row.cells.length]))
    expect(counts).toEqual({
      button: 6,
      input: 6,
      select: 7,
      switch: 6,
      checkbox: 7,
      radio: 6,
      segmented: 5,
      tag: 4,
      card: 7,
      bubble: 8,
      modal: 5,
      toast: 5,
      progress: 5,
      skeleton: 6,
      empty: 3,
    })
  })

  it('每格有状态名、行前缀标识与验收编号', () => {
    for (const row of COMPONENT_ROWS) {
      expect(row.qa).toMatch(/^QA-C-\d{2}$/)
      expect(row.rule.length).toBeGreaterThan(0)
      for (const item of row.cells) {
        expect(item.label.length).toBeGreaterThan(0)
        expect(item.id).toBe(`${row.key}-${item.slug}`)
      }
    }
  })
})

describe('视图格位', () => {
  it('9 行 60 格，与契约视图同集合', () => {
    expect(VIEW_ROWS).toHaveLength(9)
    expect(VIEW_CELL_COUNT).toBe(60)
    expect(VIEW_ROWS.map((row) => row.key).sort()).toEqual([
      'chat',
      'debug',
      'error',
      'import',
      'memory',
      'persona',
      'rules',
      'settings',
      'setup',
    ])
  })

  it('逐视图格数与设计口径一致', () => {
    const counts = Object.fromEntries(VIEW_ROWS.map((row) => [row.key, row.cells.length]))
    expect(counts).toEqual({
      chat: 9,
      memory: 8,
      persona: 8,
      rules: 5,
      import: 8,
      settings: 11,
      debug: 5,
      setup: 4,
      error: 2,
    })
  })

  it('里程碑归属覆盖 M5 / M6 / M7', () => {
    const milestones = new Set(VIEW_ROWS.map((row) => row.milestone))
    expect([...milestones].sort()).toEqual(['M5', 'M6', 'M7'])
  })
})

describe('格位标识', () => {
  it('全部格位标识唯一', () => {
    const ids = allCellIds()
    expect(ids).toHaveLength(COMPONENT_CELL_COUNT + VIEW_CELL_COUNT)
    expect(new Set(ids).size).toBe(ids.length)
  })
})

describe('视图缩略', () => {
  it('已登记视图读状态文案表', () => {
    const empty = VIEW_ROWS[0]?.cells.find((item) => item.id === 'chat-empty')
    const readonly = VIEW_ROWS[0]?.cells.find((item) => item.id === 'chat-restricted-readonly')
    expect(empty && viewThumbCopy('chat', empty).title).toBe('还没有消息')
    expect(readonly && viewThumbCopy('chat', readonly).action).toBe('开始新对话')
  })

  it('未登记视图给载体级说明，不留空白', () => {
    for (const row of VIEW_ROWS) {
      for (const item of row.cells) {
        const copy = viewThumbCopy(row.key, item)
        expect(copy.title.length).toBeGreaterThan(0)
        expect(copy.description.length).toBeGreaterThan(0)
      }
    }
  })

  it('骨架与空态形态按视图给默认值，未知视图有兜底', () => {
    expect(viewThumbSpec('chat').skeleton).toBe('bubble')
    expect(viewThumbSpec('debug').skeleton).toBe('table')
    expect(viewThumbSpec('memory').emptyVariant).toBe('action')
    expect(viewThumbSpec('unknown').emptyVariant).toBe('guide')
  })
})
