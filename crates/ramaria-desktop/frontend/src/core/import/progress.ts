/**
 * src/core/import/progress.ts — 导入进度的展示口径
 *
 * 职责:
 * - 把 `import-progress` 负载翻译成阶段标签、阶段进度、计数与预计剩余时间
 * - 生成完成页 / 续跑完成页的统计条目
 *
 * 设计特点:
 * - 纯函数，不依赖 Vue / Pinia / Tauri，组件与测试直接调用
 * - ETA 先取后端 EMA（`eta_seconds`），缺失时按已用时线性估算；样本不足显示「计算中…」
 * - 阶段条按导入模式裁剪：快速导入只有图片理解与会话摘要
 * - 进度只在 `total > 0` 时给百分比，未知总量走不定进度
 */

import type { ImportProgressPayload } from '../api/dto'

/** 可推进的阶段（`done` 为终态，不在阶段条上） */
export type ImportPhaseKey = 'vision' | 'l1' | 'l2' | 'l3'

/** 阶段条的一项 */
export interface ImportStage {
  key: ImportPhaseKey
  label: string
}

/** 阶段条的格位状态 */
export type ImportStageState = 'pending' | 'active' | 'done'

/** 进度展示数据 */
export interface ImportProgressView {
  phaseLabel: string
  /** 0~100；未知总量时为 null（不定进度） */
  percent: number | null
  /** 「已完成 / 总数」文本；未知总量为空串 */
  counts: string
  /** 预计剩余时间文本；无法估算为空串 */
  etaText: string
  /** done 阶段为 success */
  finished: boolean
}

const PHASE_ORDER: readonly ImportPhaseKey[] = ['vision', 'l1', 'l2', 'l3']

const PHASE_LABELS: Record<string, string> = {
  vision: '图片理解',
  l1: '会话摘要',
  l2: '事件提取',
  l3: '画像推断',
  done: '处理完成',
}

/** 阶段顺序下标；未知阶段按 -1 处理 */
function phaseIndex(phase: string): number {
  return PHASE_ORDER.indexOf(phase as ImportPhaseKey)
}

/**
 * 按导入模式给出阶段条。
 *
 * 说明:
 * - 快速导入不触发 L2 / L3，阶段条不展示这两项，避免长期停留在「待开始」。
 */
export function stagesForMode(mode: string): ImportStage[] {
  const keys: ImportPhaseKey[] = mode === 'deep' ? [...PHASE_ORDER] : ['vision', 'l1']
  return keys.map((key) => ({ key, label: PHASE_LABELS[key] ?? key }))
}

/** 阶段条格位状态：当前阶段之前为已完成，之后为待开始。 */
export function stageState(phase: string, stage: ImportPhaseKey): ImportStageState {
  if (phase === 'done') {
    return 'done'
  }
  const current = phaseIndex(phase)
  const target = phaseIndex(stage)
  if (current < 0 || target < 0) {
    return 'pending'
  }
  if (target < current) {
    return 'done'
  }
  return target === current ? 'active' : 'pending'
}

/** 阶段中文名 */
export function phaseLabel(phase: string): string {
  return PHASE_LABELS[phase] ?? '处理中'
}

/** 秒数转中文时长（四舍五入到秒 / 分 / 时）。 */
export function formatEta(seconds: number): string {
  const total = Math.max(0, Math.round(seconds))
  if (total < 60) {
    return `约 ${total} 秒`
  }
  if (total < 3600) {
    return `约 ${Math.round(total / 60)} 分钟`
  }
  const hours = Math.floor(total / 3600)
  const minutes = Math.round((total % 3600) / 60)
  return minutes > 0 ? `约 ${hours} 小时 ${minutes} 分钟` : `约 ${hours} 小时`
}

/**
 * 估算剩余时间文本。
 *
 * 参数:
 * - `payload`: 当前进度负载；
 * - `startedAt`: 本轮开始时间（Unix 毫秒）；
 * - `now`: 当前时间（Unix 毫秒，测试注入）。
 */
export function etaText(
  payload: ImportProgressPayload,
  startedAt: number,
  now: number,
): string {
  if (payload.phase === 'done') {
    return ''
  }
  if (typeof payload.eta_seconds === 'number' && payload.eta_seconds >= 0) {
    return payload.eta_seconds === 0 ? '即将完成' : formatEta(payload.eta_seconds)
  }
  if (payload.total <= 0) {
    return ''
  }
  if (payload.current >= payload.total) {
    return '即将完成'
  }
  if (payload.current <= 0) {
    return '计算中…'
  }
  const elapsed = Math.max(0, now - startedAt)
  if (elapsed < 2000) {
    return '计算中…'
  }
  const perItem = elapsed / payload.current
  return formatEta((perItem * (payload.total - payload.current)) / 1000)
}

/**
 * 生成进度展示数据。
 *
 * 参数:
 * - `payload`: 当前进度负载；
 * - `startedAt`: 本轮开始时间（Unix 毫秒）；
 * - `now`: 当前时间（Unix 毫秒）。
 */
export function progressView(
  payload: ImportProgressPayload,
  startedAt: number,
  now: number,
): ImportProgressView {
  const known = payload.total > 0
  const percent = known
    ? Math.min(100, Math.max(0, Math.round((payload.current / payload.total) * 100)))
    : null

  return {
    phaseLabel: phaseLabel(payload.phase),
    percent,
    counts: known ? `${payload.current} / ${payload.total}` : '',
    etaText: etaText(payload, startedAt, now),
    finished: payload.phase === 'done',
  }
}

/** 完成页统计条目 */
export interface ImportMetric {
  key: string
  label: string
  value: string
}

/**
 * 普通导入完成后的统计条目。
 *
 * 说明:
 * - L1 成功率仅在 done 事件带统计时出现；
 * - 快速模式没有 L2 / L3，不生成对应条目。
 */
export function doneMetrics(payload: ImportProgressPayload | null): ImportMetric[] {
  if (!payload || typeof payload.l1_success !== 'number') {
    return []
  }
  const success = payload.l1_success
  const failed = payload.l1_failed ?? 0
  const metrics: ImportMetric[] = [
    { key: 'l1', label: 'L1 摘要生成', value: `${success} / ${success + failed}` },
  ]
  if (payload.l2_triggered) {
    metrics.push({ key: 'l2', label: 'L2 事件提取', value: '已触发' })
  }
  if (payload.l3_triggered) {
    metrics.push({ key: 'l3', label: 'L3 画像推断', value: '已触发' })
  }
  return metrics
}

/**
 * 「继续处理」完成后的统计条目。
 *
 * 说明:
 * - `reset_count` / `vision_*` 仅续跑 done 事件携带；
 * - 缺失字段不生成条目，不补 0 以免误报。
 */
export function continueMetrics(payload: ImportProgressPayload | null): ImportMetric[] {
  if (!payload) {
    return []
  }
  const metrics: ImportMetric[] = []
  if (typeof payload.reset_count === 'number') {
    metrics.push({ key: 'reset', label: '重置待处理附件', value: String(payload.reset_count) })
  }
  if (typeof payload.vision_done === 'number') {
    const failed = payload.vision_failed ?? 0
    metrics.push({
      key: 'vision',
      label: '图片理解完成',
      value: `${payload.vision_done} / ${payload.vision_done + failed}`,
    })
  }
  if (typeof payload.l1_success === 'number') {
    const failed = payload.l1_failed ?? 0
    metrics.push({
      key: 'l1',
      label: 'L1 补跑成功',
      value: `${payload.l1_success} / ${payload.l1_success + failed}`,
    })
  }
  return metrics
}
