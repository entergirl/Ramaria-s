/**
 * src/views/rules/useRulesView.ts — 规则视图的数据编排
 *
 * 职责:
 * - 装载人格列表与当前人格的规则列表，维护列表 / 详情二级形态
 * - 承接启停、编辑、证据链读取与重试
 *
 * 设计特点:
 * - 规则按人格缓存：切回旧人格先出缓存再静默刷新，切新人格拉骨架并清旧数据
 * - 启停与编辑成功后按 id 就地替换，不整表重取；顺序随 `created_at` 保持不变
 * - 详情是页内二级形态：进入时记住列表滚动位置，返回后复位，证据链进详情才读
 * - 快速连续切人格以请求序号收口，迟到结果不覆盖当前态
 */
import { computed, nextTick, ref } from 'vue'
import { useRouter } from 'vue-router'

import { memoryApi, rulesApi } from '../../core/api'
import type { BehaviorRuleView, PersonaSummary, RuleEvidenceItem } from '../../core/api/dto'
import { errorMessage } from '../../core/errors'
import { selectPersonaUid } from '../../core/persona/select'
import {
  avoidText,
  parseAvoidInput,
  reactionError,
  ruleEditChanged,
} from '../../core/rules/format'
import { pushToast } from '../../core/toast'
import { useAppStore } from '../../stores/app'

/** 规则视图的二级形态 */
export type RulePane = 'list' | 'detail'

/** 单条规则的证据链装载状态 */
export interface EvidenceState {
  loading: boolean
  error: string
  items: RuleEvidenceItem[]
  loaded: boolean
}

/** 空白证据态 */
function emptyEvidence(): EvidenceState {
  return { loading: false, error: '', items: [], loaded: false }
}

export function useRulesView() {
  const app = useAppStore()
  const router = useRouter()

  const personas = ref<PersonaSummary[]>([])
  const personasLoading = ref(false)
  const personasError = ref('')
  const personaUid = ref<string | null>(null)
  const personaOpen = ref(false)

  /** 按人格缓存的规则列表（切回旧人格先出缓存） */
  const caches = ref<Record<string, BehaviorRuleView[]>>({})
  const rules = ref<BehaviorRuleView[]>([])
  const loading = ref(false)
  const refreshing = ref(false)
  const error = ref('')
  const booting = ref(true)

  const pane = ref<RulePane>('list')
  const activeId = ref<number | null>(null)
  const saving = ref<Record<number, boolean>>({})

  /** 详情页的编辑草稿（规则文本与禁忌词） */
  const draftReaction = ref('')
  const draftAvoid = ref('')
  const savingDraft = ref(false)
  const saveError = ref('')
  const discardOpen = ref(false)

  const evidence = ref<EvidenceState>(emptyEvidence())

  /** 列表滚动容器（渐隐滚动区），用于详情返回后复位 */
  const listScroller = ref<{
    scrollTop: () => number
    scrollTo: (top: number) => void
  } | null>(null)

  let listScroll = 0
  let loadSeq = 0
  let evidenceSeq = 0
  let initialized = false

  // ===== 派生状态 =====

  const activePersona = computed(
    () => personas.value.find((persona) => persona.uid === personaUid.value) ?? null,
  )

  const activeRule = computed(
    () => rules.value.find((rule) => rule.id === activeId.value) ?? null,
  )

  const count = computed(() => rules.value.length)

  /** 视图主态：错误 > 加载 > 空 > 默认 */
  const viewState = computed<'default' | 'loading' | 'empty' | 'error'>(() => {
    if (booting.value) {
      return 'loading'
    }
    if ((error.value !== '' || personasError.value !== '') && rules.value.length === 0) {
      return 'error'
    }
    if (loading.value && rules.value.length === 0) {
      return 'loading'
    }
    return rules.value.length === 0 ? 'empty' : 'default'
  })

  const busy = computed(
    () => booting.value || personasLoading.value || loading.value || refreshing.value,
  )

  /** 已有数据但刷新失败：列表保留，行内提示 */
  const partialError = computed(() => (rules.value.length > 0 ? error.value : ''))

  /** 错误态的原因文案（人格列表失败优先） */
  const errorReason = computed(() => error.value || personasError.value)

  const llmUnavailable = computed(
    () =>
      app.degradedReason === 'llm_unavailable' || app.degradedReason === 'both_unavailable',
  )

  /** 草稿校验：已有文本的规则不允许清空 */
  const draftValidation = computed(() =>
    activeRule.value ? reactionError(activeRule.value, draftReaction.value) : '',
  )

  /** 草稿是否有未保存改动 */
  const draftDirty = computed(() =>
    activeRule.value
      ? ruleEditChanged(activeRule.value, draftReaction.value, draftAvoid.value)
      : false,
  )

  const canSaveDraft = computed(
    () => draftDirty.value && draftValidation.value === '' && !savingDraft.value,
  )

  // ===== 装载 =====

  async function loadPersonas(): Promise<void> {
    if (personasLoading.value) {
      return
    }
    personasLoading.value = true
    personasError.value = ''
    try {
      personas.value = await memoryApi.getPersonas()
    } catch (failure) {
      personasError.value = errorMessage(failure, '人格列表读取失败')
    } finally {
      personasLoading.value = false
    }
  }

  /** 装载指定人格的规则列表（有缓存先出缓存并静默刷新） */
  async function loadRules(uid: string): Promise<void> {
    const seq = (loadSeq += 1)
    const cached = caches.value[uid] ?? null
    error.value = ''
    if (cached) {
      rules.value = cached
      refreshing.value = true
    } else {
      rules.value = []
      loading.value = true
    }

    try {
      const response = await rulesApi.list(uid)
      if (seq !== loadSeq) {
        return
      }
      caches.value = { ...caches.value, [uid]: response.rules }
      if (personaUid.value === uid) {
        rules.value = response.rules
      }
      // 详情中目标规则已不存在（外部改动后刷新）：退回列表，避免空白详情
      const missing =
        pane.value === 'detail' &&
        activeId.value !== null &&
        !response.rules.some((item) => item.id === activeId.value)
      if (missing) {
        await backToList()
      }
    } catch (failure) {
      if (seq !== loadSeq || personaUid.value !== uid) {
        return
      }
      error.value = errorMessage(failure, '规则列表读取失败')
    } finally {
      if (seq === loadSeq) {
        loading.value = false
        refreshing.value = false
      }
    }
  }

  /** 启动：人格列表 → 选定人格 → 规则列表 */
  async function init(): Promise<void> {
    if (initialized) {
      return
    }
    initialized = true
    booting.value = true
    try {
      await loadPersonas()
      if (personas.value.length === 0) {
        return
      }
      const uid = selectPersonaUid(personas.value)
      personaUid.value = uid
      if (uid) {
        await loadRules(uid)
      }
    } finally {
      booting.value = false
    }
  }

  /** 切换人格：退出详情、清证据态，装载目标人格规则 */
  async function choosePersona(uid: string): Promise<void> {
    personaOpen.value = false
    if (uid === personaUid.value) {
      return
    }
    evidenceSeq += 1
    personaUid.value = uid
    pane.value = 'list'
    activeId.value = null
    evidence.value = emptyEvidence()
    resetDraft()
    saving.value = {}
    await loadRules(uid)
  }

  /** 手动刷新当前人格的规则列表 */
  async function refresh(): Promise<void> {
    const uid = personaUid.value
    if (!uid || busy.value) {
      return
    }
    await loadRules(uid)
  }

  /** 手动重试主请求（人格列表失败时先补人格） */
  async function retry(): Promise<void> {
    if (personas.value.length === 0) {
      await loadPersonas()
    }
    if (personaUid.value === null) {
      personaUid.value = selectPersonaUid(personas.value)
    }
    const uid = personaUid.value
    if (uid) {
      await loadRules(uid)
    }
  }

  // ===== 本地替换 =====

  /** 按 id 就地替换规则（列表与缓存同时更新，保持原顺序） */
  function applyRule(next: BehaviorRuleView): void {
    const uid = personaUid.value
    if (!uid) {
      return
    }
    const replace = (list: BehaviorRuleView[]): BehaviorRuleView[] =>
      list.map((item) => (item.id === next.id ? next : item))
    rules.value = replace(rules.value)
    const cached = caches.value[uid]
    caches.value = { ...caches.value, [uid]: cached ? replace(cached) : rules.value }
  }

  function setSaving(ruleId: number, value: boolean): void {
    const next = { ...saving.value }
    if (value) {
      next[ruleId] = true
    } else {
      delete next[ruleId]
    }
    saving.value = next
  }

  // ===== 启停 =====

  async function toggleEnabled(rule: BehaviorRuleView): Promise<void> {
    if (saving.value[rule.id]) {
      return
    }
    setSaving(rule.id, true)
    try {
      const receipt = await rulesApi.setEnabled(rule.id, !rule.enabled)
      applyRule({ ...rule, enabled: receipt.enabled })
      pushToast({
        tone: 'success',
        message: `规则 #${rule.id} 已${receipt.enabled ? '启用' : '停用'}`,
      })
    } catch (failure) {
      pushToast({ tone: 'danger', message: errorMessage(failure, '切换规则状态失败') })
    } finally {
      setSaving(rule.id, false)
    }
  }

  // ===== 详情与证据 =====

  /** 草稿复位：详情进入 / 返回 / 切人格时调用 */
  function resetDraft(): void {
    draftReaction.value = ''
    draftAvoid.value = ''
    saveError.value = ''
  }

  async function openDetail(rule: BehaviorRuleView): Promise<void> {
    listScroll = listScroller.value?.scrollTop() ?? 0
    pane.value = 'detail'
    activeId.value = rule.id
    draftReaction.value = rule.reaction ?? ''
    draftAvoid.value = avoidText(rule.avoid)
    saveError.value = ''
    evidence.value = emptyEvidence()
    await loadEvidence(rule.id)
  }

  async function backToList(): Promise<void> {
    evidenceSeq += 1
    pane.value = 'list'
    activeId.value = null
    evidence.value = emptyEvidence()
    discardOpen.value = false
    resetDraft()
    await nextTick()
    listScroller.value?.scrollTo(listScroll)
  }

  /** 返回列表：有未保存改动时先确认，避免静默丢弃 */
  function requestBack(): void {
    if (savingDraft.value) {
      return
    }
    if (draftDirty.value) {
      discardOpen.value = true
      return
    }
    void backToList()
  }

  async function confirmDiscard(): Promise<void> {
    await backToList()
  }

  /** 装载证据链；`force` 用于失败后的重试 */
  async function loadEvidence(ruleId: number, force = false): Promise<void> {
    if (!force && (evidence.value.loading || evidence.value.loaded)) {
      return
    }
    const seq = (evidenceSeq += 1)
    evidence.value = { loading: true, error: '', items: [], loaded: false }
    try {
      const response = await rulesApi.evidence(ruleId)
      if (seq !== evidenceSeq) {
        return
      }
      evidence.value = { loading: false, error: '', items: response.evidence, loaded: true }
    } catch (failure) {
      if (seq !== evidenceSeq) {
        return
      }
      evidence.value = {
        loading: false,
        error: errorMessage(failure, '证据链读取失败'),
        items: [],
        loaded: false,
      }
    }
  }

  async function retryEvidence(): Promise<void> {
    const rule = activeRule.value
    if (rule) {
      await loadEvidence(rule.id, true)
    }
  }

  // ===== 编辑（详情页草稿） =====

  async function saveDraft(): Promise<void> {
    const rule = activeRule.value
    if (!rule || savingDraft.value) {
      return
    }
    const validation = reactionError(rule, draftReaction.value)
    if (validation !== '') {
      saveError.value = validation
      return
    }

    const reaction = draftReaction.value.trim()
    const currentReaction = (rule.reaction ?? '').trim()
    const reactionArg = reaction === currentReaction || reaction === '' ? null : reaction
    const avoid = parseAvoidInput(draftAvoid.value).join(',')
    const avoidArg = avoid === rule.avoid.join(',') ? null : avoid
    if (reactionArg === null && avoidArg === null) {
      pushToast({ tone: 'info', message: '没有需要保存的修改' })
      return
    }

    savingDraft.value = true
    saveError.value = ''
    try {
      const updated = await rulesApi.edit(rule.id, reactionArg, avoidArg)
      const converted = rule.source !== 'manual' && updated.source === 'manual'
      applyRule(updated)
      draftReaction.value = updated.reaction ?? ''
      draftAvoid.value = avoidText(updated.avoid)
      pushToast({
        tone: 'success',
        message: `规则 #${rule.id} 已保存${converted ? '，已转为手工' : ''}`,
      })
    } catch (failure) {
      saveError.value = errorMessage(failure, '保存规则失败')
    } finally {
      savingDraft.value = false
    }
  }

  // ===== 其它 =====

  function togglePersona(): void {
    personaOpen.value = !personaOpen.value
  }

  function goChat(): void {
    void router.push({ name: 'chat' })
  }

  /** 绑定列表滚动容器（详情返回后复位位置） */
  function bindListScroll(instance: unknown): void {
    listScroller.value =
      (instance as { scrollTop: () => number; scrollTo: (top: number) => void } | null) ?? null
  }

  return {
    personas,
    personasLoading,
    personasError,
    personaUid,
    personaOpen,
    rules,
    loading,
    refreshing,
    error,
    booting,
    pane,
    saving,
    draftReaction,
    draftAvoid,
    savingDraft,
    saveError,
    discardOpen,
    evidence,
    activePersona,
    activeRule,
    count,
    viewState,
    busy,
    partialError,
    errorReason,
    llmUnavailable,
    draftValidation,
    draftDirty,
    canSaveDraft,
    init,
    choosePersona,
    refresh,
    retry,
    toggleEnabled,
    openDetail,
    requestBack,
    confirmDiscard,
    retryEvidence,
    saveDraft,
    togglePersona,
    goChat,
    bindListScroll,
  }
}
