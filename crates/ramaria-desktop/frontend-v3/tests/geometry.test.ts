/**
 * tests/geometry.test.ts — 圆键导航几何用例
 *
 * 覆盖:
 * - 五个位置预设与窄视口收拢
 * - 主轴方向判定与紧凑档降级
 * - 子键 / 功能键位移（含紧凑档）
 * - 拖拽边界、越界收拢与阈值判定
 */

import { describe, expect, it } from 'vitest'

import {
  clampPoint,
  decideSide,
  dragBounds,
  exceedsDragThreshold,
  fnOffset,
  NAV_METRICS,
  presetPoint,
  subOffset,
} from '../src/components/nav/geometry'

describe('presetPoint', () => {
  it('底中锚点落在视口底部中心，距底与预设内缩一致', () => {
    const point = presetPoint('bottom', { width: 800, height: 600 })
    expect(point).toEqual({ x: 400, y: 600 - NAV_METRICS.presetInset })
  })

  it('四角锚点距边一致', () => {
    const viewport = { width: 1000, height: 800 }
    const inset = NAV_METRICS.presetInset
    expect(presetPoint('top-left', viewport)).toEqual({ x: inset, y: inset })
    expect(presetPoint('top-right', viewport)).toEqual({ x: 1000 - inset, y: inset })
    expect(presetPoint('bottom-left', viewport)).toEqual({ x: inset, y: 800 - inset })
    expect(presetPoint('bottom-right', viewport)).toEqual({ x: 1000 - inset, y: 800 - inset })
  })

  it('窄视口不产生负坐标', () => {
    const point = presetPoint('bottom-right', { width: 100, height: 100 })
    expect(point.x).toBeGreaterThan(0)
    expect(point.y).toBeGreaterThan(0)
  })
})

describe('decideSide', () => {
  it('上方装得下时向上排列、非紧凑', () => {
    const decision = decideSide({ x: 400, y: 468 }, 5, { width: 800, height: 600 })
    expect(decision).toEqual({ flipped: false, compact: false })
  })

  it('上方不足而下方足够时改为向下', () => {
    const decision = decideSide({ x: 400, y: 200 }, 5, { width: 800, height: 600 })
    expect(decision.flipped).toBe(true)
    expect(decision.compact).toBe(false)
  })

  it('两侧都装不下时降为紧凑档', () => {
    const decision = decideSide({ x: 300, y: 150 }, 4, { width: 600, height: 300 })
    expect(decision.compact).toBe(true)
  })

  it('极端窄窗取空间较大的一侧', () => {
    const decision = decideSide({ x: 100, y: 40 }, 6, { width: 200, height: 200 })
    expect(decision).toEqual({ flipped: true, compact: true })
  })
})

describe('subOffset', () => {
  it('向上排列时为负位移，间距按索引递增', () => {
    expect(subOffset(0, false, false)).toEqual({ x: 0, y: -NAV_METRICS.gapMain })
    expect(subOffset(2, false, false)).toEqual({
      x: 0,
      y: -(NAV_METRICS.gapMain + NAV_METRICS.stepMain * 2),
    })
  })

  it('向下排列时为正位移', () => {
    expect(subOffset(1, true, false)).toEqual({
      x: 0,
      y: NAV_METRICS.gapMain + NAV_METRICS.stepMain,
    })
  })

  it('紧凑档间距更小', () => {
    const normal = subOffset(3, false, false)
    const compact = subOffset(3, false, true)
    expect(Math.abs(compact.y)).toBeLessThan(Math.abs(normal.y))
  })
})

describe('fnOffset', () => {
  it('按角度落在正交方向且取整', () => {
    expect(fnOffset(0, false)).toEqual({ x: NAV_METRICS.fnRadius, y: 0 })
    expect(fnOffset(180, false)).toEqual({ x: -NAV_METRICS.fnRadius, y: 0 })
    expect(fnOffset(90, false).y).toBe(NAV_METRICS.fnRadius)
    expect(fnOffset(-90, false).y).toBe(-NAV_METRICS.fnRadius)
  })

  it('紧凑档半径更小', () => {
    expect(fnOffset(0, true).x).toBe(NAV_METRICS.compactFnRadius)
  })
})

describe('dragBounds / clampPoint', () => {
  it('边界按内边距内缩', () => {
    expect(dragBounds({ width: 800, height: 600 })).toEqual({
      minX: 44,
      maxX: 756,
      minY: 44,
      maxY: 556,
    })
  })

  it('极小视口不产生反向区间', () => {
    const bounds = dragBounds({ width: 60, height: 60 })
    expect(bounds.maxX).toBeGreaterThanOrEqual(bounds.minX)
    expect(bounds.maxY).toBeGreaterThanOrEqual(bounds.minY)
  })

  it('越界点被收进边界', () => {
    const bounds = dragBounds({ width: 800, height: 600 })
    expect(clampPoint({ x: 10, y: 700 }, bounds)).toEqual({ x: 44, y: 556 })
    expect(clampPoint({ x: 400, y: 300 }, bounds)).toEqual({ x: 400, y: 300 })
  })
})

describe('exceedsDragThreshold', () => {
  it('阈值内视为点击', () => {
    expect(exceedsDragThreshold({ x: 0, y: 0 }, { x: 3, y: 4 })).toBe(false)
  })

  it('超过阈值视为拖拽', () => {
    expect(exceedsDragThreshold({ x: 0, y: 0 }, { x: 4, y: 4 })).toBe(true)
  })
})
