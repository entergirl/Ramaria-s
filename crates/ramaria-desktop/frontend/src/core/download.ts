/**
 * src/core/download.ts — 模型下载横幅的展示口径
 *
 * 职责:
 * - 把 `model-download-progress` 负载翻译成横幅文案、进度值与终态
 *
 * 设计特点:
 * - 纯函数，不依赖 Vue / Pinia / Tauri，组件与测试直接调用
 * - 开始态不给数值（总量未知），失败态保留最后百分比
 */

import type { ModelDownloadProgress } from './api/dto'

/** 横幅进度组件状态 */
export type DownloadBannerState = 'running' | 'success' | 'failed'

/** 横幅展示数据 */
export interface DownloadBannerView {
  label: string
  /** 0~100；开始态为 undefined（不定进度） */
  value: number | undefined
  state: DownloadBannerState
}

/**
 * 生成横幅展示数据。
 *
 * 参数:
 * - `progress`: 当前下载进度（无下载时为 null）
 *
 * 返回:
 * - `null`: 不展示横幅；
 * - 其余: 文案 / 进度值 / 组件状态。
 */
export function downloadBannerView(
  progress: ModelDownloadProgress | null,
): DownloadBannerView | null {
  if (!progress) {
    return null
  }
  const model = progress.model ?? '嵌入模型'

  if (progress.status === 'started') {
    return { label: '正在准备下载 ' + model, value: undefined, state: 'running' }
  }
  if (progress.status === 'progress') {
    return { label: '正在下载 ' + model, value: progress.percent, state: 'running' }
  }
  if (progress.status === 'completed') {
    return { label: '模型下载完成', value: 100, state: 'success' }
  }
  return { label: '模型下载失败，可稍后重试', value: progress.percent, state: 'failed' }
}
