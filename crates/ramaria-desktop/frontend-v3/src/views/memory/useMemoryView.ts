/**
 * src/views/memory/useMemoryView.ts — 记忆视图的数据编排
 *
 * 职责:
 * - 装载人格列表与四层浏览数据，维护人格 / 页签 / 详情的界面态
 * - 承接记忆回对话跳转、快照恢复与两类后台动作（深度处理 / 重载画像）
 *
 * 设计特点:
 * - 五组查询经一次 `Promise.allSettled` 并发，部分失败保留可用数据
 * - 切人格先出该人格缓存再后台刷新；页签切换不触发请求
 * - 快速连续切人格时以请求序号收口，迟到结果不覆盖当前态
 * - 后台动作返回即提示，延迟刷新由定时器补一次，卸载时清理
 */
import { computed, nextTick, onBeforeUnmount, ref } from 'vue'
import { useRouter } from 'vue-router'

import { memoryApi, personaApi, sessionApi } from '../../core/api'
import type {
  FactList,
  MemoryEvent,
  MemoryL1,
  PersonalityProfile,
  ProfileStatus,
  SessionMessage,
  TraitEvidenceChain,
} from '../../core/api/dto'
import { errorMessage } from '../../core/errors'
import { pushToast } from '../../core/toast'
import { useMemoryStore, type MemoryTab } from '../../stores/memory'
import { usePersonaStore } from '../../stores/persona'

/** 单会话来源摘录的装载状态 */
export interface ExcerptState {
  loading: boolean
  error: string
  messages: SessionMessage[]
  loaded: boolean
}

/** 单个性格标签的证据链状态 */
export interface EvidenceState {
  loading: boolean
  error: string
  chain: TraitEvidenceChain | null
}

/** 页签选项（含计数徽标） */
export interface TabOption {
  value: MemoryTab
  label: string
  count?: number
}

/** L1 / L2 单页取数上限（后端上限 1000） */
const PAGE_LIMIT = 500

/** 后台动作后延迟刷新间隔（LLM 管线在后台跑，立即重取拿不到新数据） */
const REFRESH_DELAY_MS = 5000

/** 来源摘录条数 */
const EXCERPT_LIMIT = 3

export function useMemoryView() {
  const store = useMemoryStore()
  const personaStore = usePersonaStore()
  const router = useRouter()

  const personaOpen = ref(false)
  const detailId = ref<string | null>(null)
  const excerpts = ref<Record<string, ExcerptState>>({})
  const evidence = ref<Record<number, EvidenceState>>({})
  const processing = ref(false)
  const reloadingProfile = ref(false)
  const initialized = ref(false)
  const booting = ref(true)
  const workbench = ref<{
    scrollTop: () => number
    scrollTo: (top: number) => void
  } | null>(null)

  let pendingScroll = 0
  let loadSeq = 0
  const timers: number[] = []

  const tabOptions = computed<TabOption[]>(() => [
    { value: 'l1', label: 'L1 摘要', count: store.counts.l1 },
    { value: 'l2', label: 'L2 事件', count: store.counts.l2 },
    { value: 'l3', label: 'L3 性格', count: store.counts.l3 },
    { value: 'knowledge', label: '知识', count: store.counts.knowledge },
  ])

  const activeCount = computed(() => store.counts[store.tab])

  /** 视图主态：错误 > 加载 > 空 > 默认 */
  const viewState = computed<'default' | 'loading' | 'empty' | 'error'>(() => {
    if (booting.value || (store.loading && !store.data)) {
      return 'loading'
    }
    if ((store.error !== '' || store.personasError !== '') && !store.data) {
      return 'error'
    }
    if (!store.data) {
      return 'empty'
    }
    if (activeCount.value === 0) {
      return store.error !== '' ? 'error' : 'empty'
    }
    return 'default'
  })

  const busy = computed(() => booting.value || store.loading || store.refreshing)

  /** 深度处理入口：仅 QQ 导入来源的人格显示 */
  const canDeepProcess = computed(() => store.activePersona?.source === 'qq')

  /** 是否存在部分数据读取失败（有数据时走行内提示，不替换内容） */
  const partialError = computed(() => (store.data && store.error !== '' ? store.error : ''))

  function pickPersonaUid(snapshotUid: string | null): string | null {
    const preselect = personaStore.consumePreselect()
    const candidates = [preselect, snapshotUid, store.personaUid]
    for (const candidate of candidates) {
      if (candidate && store.personas.some((persona) => persona.uid === candidate)) {
        return candidate
      }
    }
    const fallback =
      store.personas.find((persona) => persona.kind === 'rama' && persona.is_active) ??
      store.personas.find((persona) => persona.kind === 'rama') ??
      store.personas[0]
    return fallback?.uid ?? null
  }

  async function loadPersonas(): Promise<void> {
    if (store.personas.length > 0 || store.personasLoading) {
      return
    }
    store.personasLoading = true
    store.personasError = ''
    try {
      store.personas = await memoryApi.getPersonas()
    } catch (failure) {
      store.personasError = errorMessage(failure, '人格列表读取失败')
    } finally {
      store.personasLoading = false
    }
  }

  /** 装载当前人格的四层数据（部分失败保留其余数据） */
  async function loadData(): Promise<void> {
    const uid = store.personaUid
    if (!uid) {
      return
    }
    const seq = (loadSeq += 1)
    const cached = store.data !== null
    store.error = ''
    if (cached) {
      store.refreshing = true
    } else {
      store.loading = true
    }

    try {
      const results = await Promise.allSettled([
        memoryApi.getL1(uid, PAGE_LIMIT),
        memoryApi.getL2(uid, PAGE_LIMIT),
        memoryApi.getProfile(uid),
        memoryApi.getProfileStatus(uid),
        memoryApi.getFacts(uid),
      ])

      const [l1Result, l2Result, profileResult, statusResult, factsResult] = results
      const messageOf = (result: PromiseSettledResult<unknown>, fallback: string): string =>
        result.status === 'rejected' ? errorMessage(result.reason, fallback) : ''

      const failures = results.filter((result) => result.status === 'rejected')
      const firstFailure = failures[0]
      const firstMessage = firstFailure ? messageOf(firstFailure, '记忆数据读取失败') : ''

      if (failures.length === results.length) {
        store.clearData(uid)
        if (seq === loadSeq) {
          store.error = firstMessage || '记忆数据读取失败'
        }
        return
      }

      store.setData({
        personaUid: uid,
        l1: l1Result.status === 'fulfilled' ? (l1Result.value as MemoryL1[]) : [],
        l2: l2Result.status === 'fulfilled' ? (l2Result.value as MemoryEvent[]) : [],
        profile: profileResult.status === 'fulfilled' ? (profileResult.value as PersonalityProfile) : null,
        status: statusResult.status === 'fulfilled' ? (statusResult.value as ProfileStatus) : null,
        facts: factsResult.status === 'fulfilled' ? (factsResult.value as FactList) : null,
        loadedAt: Date.now(),
      })

      if (failures.length > 0 && seq === loadSeq) {
        store.error = `部分数据未能读取：${firstMessage}`
      }
    } finally {
      if (seq === loadSeq) {
        store.loading = false
        store.refreshing = false
      }
    }

    if (seq === loadSeq && pendingScroll > 0) {
      const top = pendingScroll
      pendingScroll = 0
      await nextTick()
      workbench.value?.scrollTo(top)
    }
  }

  /** 启动：人格列表 → 选定人格 → 四层数据 → 恢复快照 */
  async function init(): Promise<void> {
    if (initialized.value) {
      return
    }
    initialized.value = true

    const snapshot = store.consumeSnapshot()
    try {
      await loadPersonas()
      if (store.personas.length === 0) {
        return
      }

      const uid = pickPersonaUid(snapshot?.personaUid ?? null)
      store.selectPersona(uid)
      if (snapshot && uid && snapshot.personaUid === uid) {
        store.setTab(snapshot.tab)
        pendingScroll = snapshot.scrollTop
      }
      await loadData()
    } finally {
      booting.value = false
    }
  }

  /** 切换人格：清空详情与摘录缓存，装载目标人格数据 */
  async function choosePersona(uid: string): Promise<void> {
    if (uid === store.personaUid) {
      return
    }
    store.selectPersona(uid)
    detailId.value = null
    excerpts.value = {}
    evidence.value = {}
    await loadData()
    personaOpen.value = false
  }

  /** 切换页签：收起详情，不触发请求 */
  function switchTab(tab: string): void {
    store.setTab(tab as MemoryTab)
    detailId.value = null
  }

  /** 展开 / 收起卡片详情（同时只开一张） */
  async function toggleDetail(id: string, sessionId?: string): Promise<void> {
    if (detailId.value === id) {
      detailId.value = null
      return
    }
    detailId.value = id
    if (sessionId) {
      await loadExcerpt(sessionId)
    }
  }

  /** 装载 L1 来源会话摘录（最近三条，失败可重试） */
  async function loadExcerpt(sessionId: string): Promise<void> {
    const current = excerpts.value[sessionId]
    if (current && (current.loading || current.loaded)) {
      return
    }
    excerpts.value = {
      ...excerpts.value,
      [sessionId]: { loading: true, error: '', messages: [], loaded: false },
    }
    try {
      const detail = await sessionApi.get(sessionId, EXCERPT_LIMIT)
      excerpts.value = {
        ...excerpts.value,
        [sessionId]: { loading: false, error: '', messages: detail.messages, loaded: true },
      }
    } catch (failure) {
      excerpts.value = {
        ...excerpts.value,
        [sessionId]: {
          loading: false,
          error: errorMessage(failure, '来源会话读取失败'),
          messages: [],
          loaded: false,
        },
      }
    }
  }

  /** 展开 / 收起性格标签详情，并把证据链一起装载 */
  async function toggleTrait(traitId: number): Promise<void> {
    const id = `l3:${traitId}`
    if (detailId.value === id) {
      detailId.value = null
      return
    }
    detailId.value = id
    await loadEvidence(traitId)
  }

  /** 装载性格标签证据链（失败保留重试入口） */
  async function loadEvidence(traitId: number): Promise<void> {
    const uid = store.personaUid
    const current = evidence.value[traitId]
    if (!uid || (current && (current.loading || current.chain !== null))) {
      return
    }
    evidence.value = {
      ...evidence.value,
      [traitId]: { loading: true, error: '', chain: null },
    }
    try {
      const chains = await memoryApi.getEvidence(uid, traitId)
      evidence.value = {
        ...evidence.value,
        [traitId]: { loading: false, error: '', chain: chains[0] ?? null },
      }
    } catch (failure) {
      evidence.value = {
        ...evidence.value,
        [traitId]: {
          loading: false,
          error: errorMessage(failure, '证据链读取失败'),
          chain: null,
        },
      }
    }
  }

  /** 查看对话：保存快照后带会话参数跳到对话视图 */
  function openChat(sessionId: string, personaUid: string | null): void {
    store.saveSnapshot(workbench.value?.scrollTop() ?? 0)
    void router.push({
      name: 'chat',
      query: {
        sessionId,
        fromView: 'memory',
        ...(personaUid ? { personaUid } : {}),
      },
    })
  }

  /** 延迟刷新当前人格（后台动作完成后调用；切走则跳过） */
  function scheduleRefresh(uid: string): void {
    const timer = window.setTimeout(() => {
      if (store.personaUid !== uid) {
        return
      }
      store.clearData(uid)
      void loadData()
    }, REFRESH_DELAY_MS)
    timers.push(timer)
  }

  /** 深度处理导入的消息：重生成 L1，无关联会话时补一次全局 L2→L3 */
  async function deepProcess(): Promise<void> {
    const uid = store.personaUid
    if (!uid || processing.value) {
      return
    }
    processing.value = true
    try {
      const receipt = await personaApi.regenerateImportPipeline(uid)
      if (receipt.early_terminated) {
        pushToast({
          tone: 'danger',
          message: `对话模型不可用，已提前终止：失败 ${receipt.l1_failed} 次，跳过 ${receipt.remaining_skipped} 个会话`,
        })
      } else if (receipt.l1_failed > 0) {
        pushToast({
          tone: 'warn',
          message: `L1 重新生成 ${receipt.l1_regenerated}/${receipt.total_sessions}，失败 ${receipt.l1_failed}；L2/L3 后台继续`,
        })
      } else if (receipt.l1_regenerated > 0) {
        pushToast({
          tone: 'success',
          message: `L1 已重新生成 ${receipt.l1_regenerated}/${receipt.total_sessions}；L2/L3 后台处理中`,
        })
      } else {
        pushToast({ tone: 'info', message: receipt.message || '该人格没有关联的消息需要处理' })
      }

      if (receipt.total_sessions === 0) {
        await memoryApi.triggerPipeline().catch(() => {
          pushToast({ tone: 'warn', message: '全局管线触发失败，可稍后重试' })
        })
      }
      scheduleRefresh(uid)
    } catch (failure) {
      pushToast({ tone: 'danger', message: errorMessage(failure, '深度处理失败') })
    } finally {
      processing.value = false
    }
  }

  /** 重载画像：触发该人格 L2→L3 后台管线 */
  async function reloadProfile(): Promise<void> {
    const uid = store.personaUid
    if (!uid || reloadingProfile.value) {
      return
    }
    reloadingProfile.value = true
    try {
      await personaApi.refresh(uid)
      pushToast({ tone: 'success', message: '已提交重算，画像将在后台更新' })
      scheduleRefresh(uid)
    } catch (failure) {
      pushToast({ tone: 'danger', message: errorMessage(failure, '重载画像失败') })
    } finally {
      reloadingProfile.value = false
    }
  }

  /** 绑定工作台实例（滚动快照读写走它） */
  function bindWorkbench(instance: unknown): void {
    workbench.value =
      (instance as { scrollTop: () => number; scrollTo: (top: number) => void } | null) ?? null
  }

  /** 手动重试主请求 */
  async function retry(): Promise<void> {
    if (store.personas.length === 0) {
      store.personasError = ''
      await loadPersonas()
      if (store.personas.length > 0 && !store.personaUid) {
        store.selectPersona(pickPersonaUid(null))
      }
    }
    await loadData()
  }

  onBeforeUnmount(() => {
    for (const timer of timers) {
      window.clearTimeout(timer)
    }
    timers.length = 0
  })

  return {
    store,
    personaOpen,
    detailId,
    excerpts,
    evidence,
    processing,
    reloadingProfile,
    workbench,
    tabOptions,
    viewState,
    busy,
    canDeepProcess,
    partialError,
    bindWorkbench,
    init,
    choosePersona,
    switchTab,
    toggleDetail,
    toggleTrait,
    loadExcerpt,
    loadEvidence,
    openChat,
    deepProcess,
    reloadProfile,
    retry,
  }
}
