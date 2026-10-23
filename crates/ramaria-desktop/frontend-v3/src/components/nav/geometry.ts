/**
 * src/components/nav/geometry.ts — 圆键导航几何
 *
 * 职责:
 * - 计算子键与功能键相对核心键的位移、主轴方向、紧凑档与拖拽边界
 *
 * 设计特点:
 * - 全部为纯函数：输入视口与锚点即得结果，不读 DOM、不写样式
 * - 常量与视觉稿 `nav-demo-v3.js` 一致，是本模块几何取值的单一源
 * - 组件只负责把结果写成 CSS 变量，几何判断不散落在组件里
 */

/** 尺寸与间距常量（与视觉稿常量一致） */
export const NAV_METRICS = {
  coreSize: 62,
  subSize: 52,
  fnSize: 48,
  brandHeight: 52,
  stepMain: 58,
  gapMain: 106,
  fnRadius: 92,
  compactStepMain: 50,
  compactGapMain: 92,
  compactFnRadius: 82,
  dragPad: 44,
  presetInset: 132,
  dragThreshold: 5,
  /** 子键半径（末键占位计算用） */
  subRadius: 26,
  /** 主轴两端余量 */
  edgeSlack: 12,
  /** 方向判定余量 */
  tailSlack: 16,
} as const

/** 导航位置预设 */
export type NavPreset = 'bottom' | 'top-left' | 'top-right' | 'bottom-left' | 'bottom-right'

/** 位置预设的显示名 */
export const NAV_PRESET_LABELS: Record<NavPreset, string> = {
  bottom: '底中',
  'top-left': '左上',
  'top-right': '右上',
  'bottom-left': '左下',
  'bottom-right': '右下',
}

/** 二维点 */
export interface Point {
  x: number
  y: number
}

/** 视口尺寸 */
export interface Size {
  width: number
  height: number
}

/** 拖拽边界 */
export interface Bounds {
  minX: number
  maxX: number
  minY: number
  maxY: number
}

/** 档位参数 */
export interface NavDensity {
  compact: boolean
  stepMain: number
  gapMain: number
  fnRadius: number
}

/** 方向与档位判定结果 */
export interface SideDecision {
  /** true 表示子键向下排列（品牌键随之移到上方） */
  flipped: boolean
  compact: boolean
}

/** 取档位参数 */
export function density(compact: boolean): NavDensity {
  if (compact) {
    return {
      compact: true,
      stepMain: NAV_METRICS.compactStepMain,
      gapMain: NAV_METRICS.compactGapMain,
      fnRadius: NAV_METRICS.compactFnRadius,
    }
  }
  return {
    compact: false,
    stepMain: NAV_METRICS.stepMain,
    gapMain: NAV_METRICS.gapMain,
    fnRadius: NAV_METRICS.fnRadius,
  }
}

/** 位置预设对应的锚点（核心键中心） */
export function presetPoint(preset: NavPreset, viewport: Size): Point {
  const inset = NAV_METRICS.presetInset
  const width = viewport.width
  const height = viewport.height
  if (preset === 'top-left') {
    return { x: inset, y: inset }
  }
  if (preset === 'top-right') {
    return { x: Math.max(inset, width - inset), y: inset }
  }
  if (preset === 'bottom-left') {
    return { x: inset, y: Math.max(inset, height - inset) }
  }
  if (preset === 'bottom-right') {
    return { x: Math.max(inset, width - inset), y: Math.max(inset, height - inset) }
  }
  return { x: Math.round(width / 2), y: Math.max(inset, height - inset) }
}

/**
 * 判定主轴方向与档位。
 *
 * 判据:
 * 1. 上方装得下 → 向上；否则下方装得下 → 向下；
 * 2. 两侧都装不下 → 降紧凑档再判定；
 * 3. 紧凑档仍不足 → 取空间较大的一侧。
 */
export function decideSide(anchor: Point, count: number, viewport: Size): SideDecision {
  const items = Math.max(count, 1)
  const need =
    NAV_METRICS.gapMain +
    NAV_METRICS.stepMain * (items - 1) +
    NAV_METRICS.subRadius +
    NAV_METRICS.tailSlack
  const needCompact =
    NAV_METRICS.compactGapMain +
    NAV_METRICS.compactStepMain * (items - 1) +
    NAV_METRICS.subRadius -
    3 +
    NAV_METRICS.edgeSlack

  const spaceUp = anchor.y - NAV_METRICS.edgeSlack
  const spaceDown = viewport.height - anchor.y - NAV_METRICS.edgeSlack

  if (spaceUp >= need) {
    return { flipped: false, compact: false }
  }
  if (spaceDown >= need) {
    return { flipped: true, compact: false }
  }
  if (spaceUp >= needCompact) {
    return { flipped: false, compact: true }
  }
  if (spaceDown >= needCompact) {
    return { flipped: true, compact: true }
  }
  return { flipped: spaceDown > spaceUp, compact: true }
}

/** 子键位移（`index` 自 0 起） */
export function subOffset(index: number, flipped: boolean, compact: boolean): Point {
  const { stepMain, gapMain } = density(compact)
  const distance = gapMain + stepMain * index
  return { x: 0, y: flipped ? distance : -distance }
}

/** 功能键位移：0 右 / 90 下 / 180 左 / -90 上 */
export function fnOffset(angleDeg: number, compact: boolean): Point {
  const { fnRadius } = density(compact)
  const rad = (angleDeg * Math.PI) / 180
  return {
    x: Math.round(Math.cos(rad) * fnRadius),
    y: Math.round(Math.sin(rad) * fnRadius),
  }
}

/** 核心键中心允许移动的范围 */
export function dragBounds(viewport: Size, pad = NAV_METRICS.dragPad): Bounds {
  return {
    minX: pad,
    maxX: Math.max(pad, viewport.width - pad),
    minY: pad,
    maxY: Math.max(pad, viewport.height - pad),
  }
}

/** 把点收进边界 */
export function clampPoint(point: Point, bounds: Bounds): Point {
  return {
    x: Math.min(Math.max(point.x, bounds.minX), bounds.maxX),
    y: Math.min(Math.max(point.y, bounds.minY), bounds.maxY),
  }
}

/** 位移是否越过拖拽阈值（区分点击与拖拽） */
export function exceedsDragThreshold(
  from: Point,
  to: Point,
  threshold = NAV_METRICS.dragThreshold,
): boolean {
  const dx = to.x - from.x
  const dy = to.y - from.y
  return Math.sqrt(dx * dx + dy * dy) > threshold
}
