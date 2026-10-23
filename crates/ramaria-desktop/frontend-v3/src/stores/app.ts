/**
 * src/stores/app.ts — 应用状态与索引状态
 *
 * 职责:
 * - 承载 AppState 与 IndexStatus 两份状态，派生全屏判据与索引降级判据
 * - 启动引导：并行读取应用状态与索引状态，降级时补取降级原因
 * - 订阅 `index-progress` 推进索引状态
 *
 * 设计特点:
 * - IndexStatus 独立于 AppState：其变化只影响横幅与设置卡，不切视图
 * - 全屏判据与索引判据只在此处计算，视图不重复判断
 * - 引导失败只记录文案，不改变 AppState（避免把读取失败误判成 fatal）
 */

import { defineStore } from 'pinia'
import { computed, ref } from 'vue'

import { chatApi, indexApi, setupApi } from '../core/api'
import type { AppStateValue, DegradedReason, IndexProgress, IndexStatusView } from '../core/api/dto'
import { errorMessage } from '../core/errors'
import { EVENTS, useEvent } from '../core/events'

export const useAppStore = defineStore('app', () => {
  const appState = ref<AppStateValue>('needs_setup')
  const degradedReason = ref<DegradedReason | null>(null)
  const indexStatus = ref<IndexStatusView | null>(null)
  const booting = ref(false)
  const bootError = ref('')

  /** 全屏态：渲染全屏页，隐藏导航框架 */
  const fullscreen = computed(
    () => appState.value === 'needs_setup' || appState.value === 'fatal_error',
  )

  /** 索引尚未就绪（chat 横幅判据：idle / building / degraded） */
  const indexNotReady = computed(() => {
    const status = indexStatus.value?.status
    return status === 'idle' || status === 'building' || status === 'degraded'
  })

  /** 索引就绪但存在补算失败（部分向量缺失） */
  const indexPartial = computed(
    () => indexStatus.value?.status === 'ready' && (indexStatus.value?.failed ?? 0) > 0,
  )

  /** 启动引导：应用状态 + 索引状态，降级时补取原因 */
  async function bootstrap(): Promise<void> {
    if (booting.value) {
      return
    }
    booting.value = true
    bootError.value = ''
    try {
      const [state, index] = await Promise.all([chatApi.getAppState(), indexApi.getStatus()])
      appState.value = state
      indexStatus.value = index
      degradedReason.value = state === 'degraded' ? await setupApi.getDegradedReason() : null
    } catch (error) {
      bootError.value = errorMessage(error, '启动状态读取失败')
    } finally {
      booting.value = false
    }
  }

  /** 重新读取应用状态（首次配置完成后调用） */
  async function refreshAppState(): Promise<void> {
    try {
      appState.value = await setupApi.refresh()
      degradedReason.value = appState.value === 'degraded' ? await setupApi.getDegradedReason() : null
    } catch (error) {
      bootError.value = errorMessage(error, '状态刷新失败')
    }
  }

  /** 重新读取索引状态 */
  async function refreshIndex(): Promise<void> {
    try {
      indexStatus.value = await indexApi.getStatus()
    } catch (error) {
      bootError.value = errorMessage(error, '索引状态读取失败')
    }
  }

  /** 触发索引重建；返回是否本次启动（已有构建在途时为 false） */
  async function rebuildIndex(): Promise<boolean> {
    try {
      const receipt = await indexApi.rebuild()
      if (receipt.started) {
        await refreshIndex()
      }
      return receipt.started
    } catch (error) {
      bootError.value = errorMessage(error, '索引重建失败')
      return false
    }
  }

  function applyIndexProgress(progress: IndexProgress): void {
    const previous = indexStatus.value
    indexStatus.value = {
      status: progress.status,
      loaded: progress.loaded,
      total: progress.total,
      failed: progress.failed,
      percent: progress.percent,
      model: progress.model ?? previous?.model,
      last_error: progress.status === 'degraded' ? previous?.last_error : undefined,
      updated_at: Date.now(),
    }
  }

  useEvent(EVENTS.indexProgress, applyIndexProgress)

  return {
    appState,
    degradedReason,
    indexStatus,
    booting,
    bootError,
    fullscreen,
    indexNotReady,
    indexPartial,
    bootstrap,
    refreshAppState,
    refreshIndex,
    rebuildIndex,
  }
})
