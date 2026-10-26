/**
 * src/views/debug/useDebugView.ts — 调试视图的数据编排
 *
 * 职责:
 * - 维护三个页签（关键词池 / 说话风格 / 评估产物）的取数、当前页签与视图主态
 * - 承接别名裁决、人格切换、产物目录选择与单文件解析
 *
 * 设计特点:
 * - 页签按需装载一次，之后留在内存，切换不重复请求
 * - 页签是路由参数，刷新与深链都能回到同一页签
 * - 主态只由一个页签决定：错误 > 加载 > 空 > 默认；评估目录不可读属降级，不替换内容
 * - 快速切换人格时用请求序号收口，迟到的统计结果不覆盖当前人格
 */

import { computed, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'

import { evaluationApi, keywordsApi, memoryApi, styleApi } from '../../core/api'
import type {
  EvalFile,
  KeywordPool,
  PendingAlias,
  PersonaSummary,
  StyleStats,
} from '../../core/api/dto'
import { buildStyleModel, type StyleStatsModel } from '../../core/debug/format'
import { summarizeArtifact, type ArtifactSummary } from '../../core/debug/probe'
import { errorMessage } from '../../core/errors'
import { selectPersonaUid } from '../../core/persona/select'
import { pushToast } from '../../core/toast'
import { useDebugStore } from '../../stores/debug'

/** 调试页签 */
export type DebugTab = 'keywords' | 'style' | 'eval'

/** 页签顺序（工具栏与路由校验共用） */
export const DEBUG_TABS: Array<{ value: DebugTab; label: string }> = [
  { value: 'keywords', label: '关键词池' },
  { value: 'style', label: '说话风格' },
  { value: 'eval', label: '评估产物' },
]

/** 视图主态 */
export type DebugViewState = 'default' | 'loading' | 'empty' | 'error'

/** 已打开的产物摘要 */
export interface ArtifactView {
  name: string
  summary: ArtifactSummary
}

/** 默认页签 */
const DEFAULT_TAB: DebugTab = 'keywords'

function isDebugTab(value: string): value is DebugTab {
  return DEBUG_TABS.some((tab) => tab.value === value)
}

export function useDebugView() {
  const route = useRoute()
  const router = useRouter()
  const debugStore = useDebugStore()

  const tab = ref<DebugTab>(DEFAULT_TAB)
  const loadedTabs = new Set<DebugTab>()

  // ---- 关键词池 ----
  const pool = ref<KeywordPool | null>(null)
  const pendingAliases = ref<PendingAlias[]>([])
  const keywordsLoading = ref(false)
  const keywordsError = ref('')
  const resolving = ref<Record<string, boolean>>({})

  // ---- 说话风格 ----
  const personas = ref<PersonaSummary[]>([])
  const personaUid = ref<string | null>(null)
  const styleStats = ref<StyleStats | null>(null)
  const styleModel = ref<StyleStatsModel | null>(null)
  const personasLoading = ref(false)
  const personasError = ref('')
  const styleLoading = ref(false)
  const styleError = ref('')
  let styleSeq = 0

  // ---- 评估产物 ----
  const evalDir = computed(() => debugStore.evalDir)
  const evalFiles = ref<EvalFile[]>([])
  const evalLoading = ref(false)
  const evalDirError = ref('')
  const artifact = ref<ArtifactView | null>(null)
  const artifactLoading = ref(false)
  const artifactError = ref('')

  // =========================================================
  // 主态
  // =========================================================

  const viewState = computed<DebugViewState>(() => {
    if (tab.value === 'keywords') {
      if (keywordsLoading.value && !pool.value) {
        return 'loading'
      }
      if (keywordsError.value !== '' && !pool.value) {
        return 'error'
      }
      if (pool.value && pool.value.total === 0 && pendingAliases.value.length === 0) {
        return 'empty'
      }
      return 'default'
    }
    if (tab.value === 'style') {
      if (personasError.value !== '' || (styleError.value !== '' && !styleStats.value)) {
        return 'error'
      }
      if ((personasLoading.value && personas.value.length === 0) || styleLoading.value) {
        return 'loading'
      }
      if (personas.value.length === 0 || !styleStats.value) {
        return 'empty'
      }
      return 'default'
    }
    if (artifactError.value !== '') {
      return 'error'
    }
    if (artifactLoading.value || (evalLoading.value && evalFiles.value.length === 0)) {
      return 'loading'
    }
    if (evalDir.value === '' || evalFiles.value.length === 0) {
      return 'empty'
    }
    return 'default'
  })

  /** 当前页签是否在请求中（工具栏刷新键的加载态） */
  const busy = computed(() => {
    if (tab.value === 'keywords') {
      return keywordsLoading.value
    }
    if (tab.value === 'style') {
      return styleLoading.value || personasLoading.value
    }
    return evalLoading.value || artifactLoading.value
  })

  /** 主请求失败的一句话原因 */
  const errorReason = computed(() => {
    if (tab.value === 'keywords') {
      return keywordsError.value
    }
    if (tab.value === 'style') {
      return personasError.value || styleError.value
    }
    return artifactError.value
  })

  /** 已有数据时刷新失败：行内提示，不替换内容 */
  const partialError = computed(() => {
    if (tab.value === 'keywords') {
      return pool.value && keywordsError.value !== '' ? keywordsError.value : ''
    }
    if (tab.value === 'style') {
      return styleStats.value && styleError.value !== '' ? styleError.value : ''
    }
    return ''
  })

  /** 评估目录不可读（降级，不替换内容） */
  const degradedDir = computed(() => (tab.value === 'eval' ? evalDirError.value : ''))

  // =========================================================
  // 关键词池
  // =========================================================

  async function loadKeywords(): Promise<void> {
    if (keywordsLoading.value) {
      return
    }
    keywordsLoading.value = true
    keywordsError.value = ''
    try {
      const [poolResult, pendingResult] = await Promise.all([
        keywordsApi.list(),
        keywordsApi.pendingAliases(),
      ])
      pool.value = poolResult
      pendingAliases.value = pendingResult.aliases
    } catch (failure) {
      keywordsError.value = errorMessage(failure, '关键词池读取失败')
    } finally {
      keywordsLoading.value = false
    }
  }

  /**
   * 裁决一个待确认别名。
   *
   * 参数:
   * - `alias`: 别名文本
   * - `action`: confirm 合并到规范词 / reject 晋升为独立规范词
   */
  async function resolveAlias(alias: string, action: 'confirm' | 'reject'): Promise<void> {
    if (resolving.value[alias]) {
      return
    }
    resolving.value = { ...resolving.value, [alias]: true }
    try {
      const result = await keywordsApi.resolveAlias(alias, action)
      if (result.already_applied) {
        pushToast({ tone: 'info', message: `${alias} 此前已合并，无需重复确认` })
      } else {
        const verb = action === 'confirm' ? '已合并到规范词' : '已晋升为规范词'
        pushToast({ tone: 'success', message: `${alias} ${verb}` })
      }
      await loadKeywords()
    } catch (failure) {
      pushToast({ tone: 'danger', message: errorMessage(failure, '别名处理失败') })
    } finally {
      const next = { ...resolving.value }
      delete next[alias]
      resolving.value = next
    }
  }

  // =========================================================
  // 说话风格
  // =========================================================

  async function loadPersonas(): Promise<void> {
    if (personasLoading.value || personas.value.length > 0) {
      return
    }
    personasLoading.value = true
    personasError.value = ''
    try {
      personas.value = await memoryApi.getPersonas()
      if (!personaUid.value) {
        personaUid.value = selectPersonaUid(personas.value)
      }
    } catch (failure) {
      personasError.value = errorMessage(failure, '人格列表读取失败')
    } finally {
      personasLoading.value = false
    }
  }

  async function loadStyle(): Promise<void> {
    const uid = personaUid.value
    if (!uid) {
      return
    }
    styleSeq += 1
    const seq = styleSeq
    styleLoading.value = true
    styleError.value = ''
    try {
      const stats = await styleApi.getStats(uid)
      if (seq !== styleSeq) {
        return
      }
      styleStats.value = stats
      styleModel.value = stats ? buildStyleModel(stats) : null
    } catch (failure) {
      if (seq !== styleSeq) {
        return
      }
      styleStats.value = null
      styleModel.value = null
      styleError.value = errorMessage(failure, '说话风格统计读取失败')
    } finally {
      if (seq === styleSeq) {
        styleLoading.value = false
      }
    }
  }

  /** 切换展示人格并重新取数 */
  async function selectStylePersona(uid: string): Promise<void> {
    if (uid === personaUid.value) {
      return
    }
    personaUid.value = uid
    styleStats.value = null
    styleModel.value = null
    await loadStyle()
  }

  async function refreshStyle(): Promise<void> {
    await loadPersonas()
    await loadStyle()
  }

  // =========================================================
  // 评估产物
  // =========================================================

  async function loadEvalFiles(): Promise<void> {
    if (evalDir.value === '') {
      evalFiles.value = []
      evalDirError.value = ''
      return
    }
    evalLoading.value = true
    evalDirError.value = ''
    try {
      const result = await evaluationApi.listFiles(evalDir.value)
      evalFiles.value = result.files
    } catch (failure) {
      evalFiles.value = []
      evalDirError.value = errorMessage(failure, '评估目录读取失败')
    } finally {
      evalLoading.value = false
    }
  }

  /** 选择产物目录；取消时保持现状 */
  async function pickEvalDir(): Promise<void> {
    try {
      const picked = await evaluationApi.pickDir()
      if (!picked) {
        return
      }
      debugStore.evalDir = picked
      artifact.value = null
      artifactError.value = ''
      await loadEvalFiles()
    } catch (failure) {
      pushToast({ tone: 'danger', message: errorMessage(failure, '目录选择失败') })
    }
  }

  /** 解析单个产物文件；题面与模型回复不进视图 */
  async function openArtifact(file: EvalFile): Promise<void> {
    if (artifactLoading.value) {
      return
    }
    artifactLoading.value = true
    artifactError.value = ''
    try {
      const value = await evaluationApi.readResult(file.path)
      artifact.value = { name: file.name, summary: summarizeArtifact(value) }
    } catch (failure) {
      artifact.value = null
      artifactError.value = errorMessage(failure, '产物读取失败')
    } finally {
      artifactLoading.value = false
    }
  }

  function closeArtifact(): void {
    artifact.value = null
    artifactError.value = ''
  }

  // =========================================================
  // 页签与路由
  // =========================================================

  function routeParamTab(): string {
    const value = route.params.tab
    return Array.isArray(value) ? (value[0] ?? '') : String(value ?? '')
  }

  /** 路由 → 页签：非法页签收敛到默认页签并回写路由 */
  function syncFromRoute(): DebugTab {
    const raw = routeParamTab()
    const next = isDebugTab(raw) ? raw : DEFAULT_TAB
    if (raw !== next) {
      void router.replace({ name: 'debug', params: { tab: next } })
    }
    tab.value = next
    return next
  }

  /** 切页签只改路由，由路由回写状态，避免两套来源分叉 */
  function selectTab(next: string): void {
    if (!isDebugTab(next) || next === tab.value) {
      return
    }
    void router.replace({ name: 'debug', params: { tab: next } })
  }

  /** 首次进入某页签时装载数据 */
  async function ensureTab(which: DebugTab): Promise<void> {
    if (loadedTabs.has(which)) {
      return
    }
    loadedTabs.add(which)
    if (which === 'keywords') {
      await loadKeywords()
      return
    }
    if (which === 'style') {
      await refreshStyle()
      return
    }
    await loadEvalFiles()
  }

  /** 装载当前页签（供路由变化与重试用） */
  async function ensureCurrent(): Promise<void> {
    await ensureTab(tab.value)
  }

  /** 刷新当前页签 */
  async function refresh(): Promise<void> {
    if (tab.value === 'keywords') {
      await loadKeywords()
      return
    }
    if (tab.value === 'style') {
      await refreshStyle()
      return
    }
    if (artifact.value) {
      artifact.value = null
    }
    await loadEvalFiles()
  }

  /** 重试主请求失败（错误块的动作） */
  async function retry(): Promise<void> {
    if (tab.value === 'eval' && artifactError.value !== '') {
      closeArtifact()
      return
    }
    await refresh()
  }

  return {
    // 页签
    tab,
    tabs: DEBUG_TABS,
    viewState,
    busy,
    errorReason,
    partialError,
    degradedDir,
    syncFromRoute,
    selectTab,
    ensureCurrent,
    refresh,
    retry,
    // 关键词池
    pool,
    pendingAliases,
    resolving,
    resolveAlias,
    // 说话风格
    personas,
    personaUid,
    styleModel,
    selectStylePersona,
    // 评估产物
    evalDir,
    evalFiles,
    evalDirError,
    artifact,
    artifactLoading,
    artifactError,
    pickEvalDir,
    openArtifact,
    closeArtifact,
    loadEvalFiles,
  }
}
