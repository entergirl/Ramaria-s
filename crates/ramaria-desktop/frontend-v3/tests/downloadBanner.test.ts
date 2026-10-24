/**
 * tests/downloadBanner.test.ts — 模型下载横幅的展示口径
 *
 * 覆盖:
 * - 无下载不展示
 * - 开始 / 进行中 / 完成 / 失败四态的文案、进度值与组件状态
 */

import { describe, expect, it } from 'vitest'

import { downloadBannerView } from '../src/core/download'
import type { ModelDownloadProgress } from '../src/core/api/dto'

const base: ModelDownloadProgress = {
  status: 'progress',
  downloaded: 30,
  total: 100,
  percent: 30,
  elapsedMs: 1200,
  model: 'bge-small-zh-v1.5',
}

describe('模型下载横幅', () => {
  it('无下载时不展示', () => {
    expect(downloadBannerView(null)).toBeNull()
  })

  it('开始态为不定进度', () => {
    const view = downloadBannerView({ ...base, status: 'started', percent: 0 })
    expect(view?.value).toBeUndefined()
    expect(view?.state).toBe('running')
    expect(view?.label).toContain('正在准备下载')
  })

  it('进行中展示百分比', () => {
    const view = downloadBannerView(base)
    expect(view).toEqual({
      label: '正在下载 bge-small-zh-v1.5',
      value: 30,
      state: 'running',
    })
  })

  it('完成态为成功并满格', () => {
    const view = downloadBannerView({ ...base, status: 'completed', percent: 100 })
    expect(view).toEqual({ label: '模型下载完成', value: 100, state: 'success' })
  })

  it('失败态保留最后百分比并可重试', () => {
    const view = downloadBannerView({ ...base, status: 'failed', percent: 42 })
    expect(view?.state).toBe('failed')
    expect(view?.value).toBe(42)
    expect(view?.label).toContain('可稍后重试')
  })
})
