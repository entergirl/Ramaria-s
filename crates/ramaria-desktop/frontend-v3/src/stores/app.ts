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

import { chatApi, configApi, indexApi, setupApi } from '../core/api'
import type { AppStateValue, DegradedReason, IndexProgress, IndexStatusView } from '../core/api/dto'
import { errorMessage } from '../core/errors'
import { EVENTS, useEvent } from '../core/events'
import { isSetupCompleted } from '../core/setupWizard'

export const useAppStore = defineStore('app', () => {
  const appState = ref<AppStateValue>('needs_setup')
  const degradedReason = ref<DegradedReason | null>(null)
  const indexStatus = ref<IndexStatusView | null>(null)
  const booting = ref(false)
  /** 首次引导是否已结束（成功或失败都置真，避免重复请求） */
  const booted = ref(false)
  const bootError = ref('')

  /** 索引尚未就绪（chat 横幅判据：idle / building / degraded） */
  const indexNotReady = computed(() => {
    const status = indexStatus.value?.status
    return status === 'idle' || status === 'building' || status === 'degraded'
  })

  /** 索引就绪但存在补算失败（部分向量缺失） */
  const indexPartial = computed(
    () => indexStatus.value?.status === 'ready' && (indexStatus.value?.failed ?? 0) > 0,
  )

  /**
   * 冷启动时校正「就绪」判定。
   *
   * 说明:
   * - 后端只按「后端配置存在 + 模型已选」判定就绪，测试连接也会写入后端配置，
   *   因此未走完向导的库会被误判为就绪、直接落到空白主界面
   * - 以 `settings` 表的完成标记为准：标记缺失时回到首次配置
   * - 标记读取失败时不改判定，避免把瞬时读取错误当成「未配置」
   */
  async function resolveSetupState(state: AppStateValue): Promise<AppStateValue> {
    if (state !== 'ready' && state !== 'degraded') {
      return state
    }
    try {
      const settings = await configApi.getSettings()
      return isSetupCompleted(settings) ? state : 'needs_setup'
    } catch {
      return state
    }
  }

  /**
   * 启动引导：应用状态 + 索引状态，降级时补取原因。
   *
   * 参数:
   * - `force`: 已引导过也重新读取（错误页「重试」使用）
   */
  async function bootstrap(force = false): Promise<void> {
    if (booting.value || (booted.value && !force)) {
      return
    }
    booting.value = true
    bootError.value = ''
    try {
      const [state, index] = await Promise.all([chatApi.getAppState(), indexApi.getStatus()])
      indexStatus.value = index
      appState.value = await resolveSetupState(state)
      degradedReason.value =
        appState.value === 'degraded' ? await setupApi.getDegradedReason() : null
    } catch (error) {
      bootError.value = errorMessage(error, '启动状态读取失败')
    } finally {
      booting.value = false
      booted.value = true
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
    booted,
    bootError,
    indexNotReady,
    indexPartial,
    bootstrap,
    refreshAppState,
    refreshIndex,
    rebuildIndex,
  }
})
