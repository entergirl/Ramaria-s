/**
 * src/views/persona/usePersonaView.ts — 人格视图的数据编排
 *
 * 职责:
 * - 装载人格列表与默认人格标记，维护列表 / 详情二级形态与编辑草稿
 * - 承接保存、设默认、重载画像、导出、提示词预览与画像状态读取
 *
 * 设计特点:
 * - 列表与画像状态留在 store：返回列表不重新取数，保存后按 uid 局部替换
 * - 未保存修改离开详情前先确认；确认后丢弃草稿并回到列表
 * - 重载画像后延迟刷新画像状态（后台管线不会立即产出）
 * - 提示词静态段随详情打开即读取（折叠态要能读到内容），全量档需示例输入
 */

import { computed, onBeforeUnmount, ref } from 'vue'

import {
  configApi,
  memoryApi,
  personaApi,
  systemApi,
} from '../../core/api'
import type {
  PersonaPromptPreview,
  PersonaUpdateRequest,
  PresetImportOutcome,
  ProfileStatus,
} from '../../core/api/dto'
import { errorMessage } from '../../core/errors'
import type { TagTone } from '../../core/memory/format'
import { profileStatusLabel, profileStatusTone } from '../../core/memory/format'
import {
  clampText,
  DESCRIPTION_MAX,
  draftChanged,
  draftOf,
  nameError,
  NAME_MAX,
  type PersonaDraft,
} from '../../core/persona/draft'
import { exportReceiptText, importOutcomeText } from '../../core/persona/format'
import { stateCopy } from '../../core/stateCopy'
import { pushToast } from '../../core/toast'
import { useAppStore } from '../../stores/app'
import { usePersonaStore } from '../../stores/persona'
import { useSessionStore } from '../../stores/session'

/** 提示词档位 */
export type PromptTier = 'static' | 'full'

/** 画像状态刷新延迟（后台管线执行需要时间） */
const STATUS_REFRESH_DELAY_MS = 5000

export function usePersonaView() {
  const store = usePersonaStore()
  const app = useAppStore()
  const session = useSessionStore()

  const booting = ref(true)
  const initialized = ref(false)
  const saving = ref(false)
  const saveError = ref('')
  const defaultSaving = ref(false)

  const draft = ref<PersonaDraft | null>(null)
  const discardOpen = ref(false)
  const reloadOpen = ref(false)
  const reloadingProfile = ref(false)
  const exportOpen = ref(false)
  const exporting = ref(false)
  const exportError = ref('')
  const importOpen = ref(false)

  const promptOpen = ref(false)
  const promptTier = ref<PromptTier>('static')
  const promptInput = ref('')
  const promptStatic = ref<PersonaPromptPreview | null>(null)
  const promptFull = ref<PersonaPromptPreview | null>(null)
  const promptLoading = ref(false)
  const promptError = ref('')
  const promptHint = ref('')
  const promptCopied = ref(false)

  let statusTimer = 0

  const active = computed(() => {
    const uid = store.activeUid
    if (!uid) {
      return null
    }
    return store.personas.find((persona) => persona.uid === uid) ?? null
  })

  const isDefault = computed(
    () => !!store.activeUid && store.defaultUid === store.activeUid,
  )

  const defaultCount = computed(
    () => store.personas.filter((persona) => persona.uid === store.defaultUid).length,
  )

  /** 列表主态：错误 > 加载 > 空 > 默认 */
  const viewState = computed<'default' | 'loading' | 'empty' | 'error'>(() => {
    if (booting.value) {
      return 'loading'
    }
    if (store.listError !== '' && store.personas.length === 0) {
      return 'error'
    }
    if (store.personas.length > 0) {
      return 'default'
    }
    return store.listLoaded ? 'empty' : 'loading'
  })

  const busy = computed(() => booting.value || store.listLoading)

  /** 已有数据但读取失败：列表保留，行内提示 */
  const partialError = computed(() =>
    store.personas.length > 0 ? store.listError : '',
  )

  const dirty = computed(() => {
    const persona = active.value
    const current = draft.value
    if (!persona || !current) {
      return false
    }
    return draftChanged(current, persona)
  })

  const canSave = computed(() => {
    const current = draft.value
    return !!current && dirty.value && nameError(current.name) === '' && !saving.value
  })

  const llmUnavailable = computed(
    () =>
      app.degradedReason === 'llm_unavailable' || app.degradedReason === 'both_unavailable',
  )

  const profileState = computed(() =>
    active.value ? store.statusStateOf(active.value.uid) : 'idle',
  )

  const profile = computed<ProfileStatus | null>(() => {
    const uid = active.value?.uid
    if (!uid) {
      return null
    }
    return store.statusCache[uid] ?? null
  })

  /** 画像未生成（读取失败或暂无有效特质） */
  const profileDegraded = computed(() => {
    if (profileState.value === 'error') {
      return true
    }
    const hit = profile.value
    return hit !== null && hit.active_trait_count === 0
  })

  const profileChip = computed<{ text: string; tone: TagTone }>(() => {
    if (profileDegraded.value) {
      return { text: '画像尚未生成', tone: 'neutral' }
    }
    const hit = profile.value
    if (!hit) {
      return { text: '画像状态读取中…', tone: 'neutral' }
    }
    return { text: `L3 画像 · ${profileStatusLabel(hit.status)}`, tone: profileStatusTone(hit.status) }
  })

  const profileNote = computed(() => {
    const uid = active.value?.uid ?? ''
    if (profileState.value === 'error') {
      return store.statusErrors[uid] || '画像状态读取失败'
    }
    if (profileDegraded.value) {
      return stateCopy('persona.degraded.profile').description
    }
    const hit = profile.value
    if (!hit) {
      return '画像状态读取中…'
    }
    return `样本 ${hit.n_total_eff} · 特质 ${hit.active_trait_count} · ${hit.status_text}`
  })

  // ===== 装载 =====

  async function loadList(): Promise<void> {
    if (store.listLoading) {
      return
    }
    store.listLoading = true
    store.listError = ''
    try {
      store.personas = await personaApi.listFull()
      store.listLoaded = true
      // 详情形态下目标人格已不存在时退回列表，避免空白详情
      const missing =
        store.pane === 'detail' &&
        !!store.activeUid &&
        !store.personas.some((persona) => persona.uid === store.activeUid)
      if (missing) {
        doBackToList()
      }
    } catch (failure) {
      store.listError = errorMessage(failure, '人格列表读取失败')
    } finally {
      store.listLoading = false
    }
  }

  async function loadDefault(): Promise<void> {
    if (store.defaultLoaded) {
      return
    }
    store.defaultError = ''
    try {
      const settings = await configApi.getSettings()
      const hit = settings.find((item) => item.key === 'default_persona_uid')
      store.defaultUid = hit && hit.value ? hit.value : null
      store.defaultLoaded = true
    } catch (failure) {
      store.defaultError = errorMessage(failure, '默认人格读取失败')
    }
  }

  async function loadStatus(uid: string, force = false): Promise<void> {
    if (!force && store.statusStateOf(uid) !== 'idle') {
      return
    }
    store.statusLoading = { ...store.statusLoading, [uid]: true }
    try {
      store.setStatus(uid, await memoryApi.getProfileStatus(uid))
    } catch (failure) {
      store.setStatus(uid, null, errorMessage(failure, '画像状态读取失败'))
    } finally {
      const next = { ...store.statusLoading }
      delete next[uid]
      store.statusLoading = next
    }
  }

  /** 启动：列表与默认标记并行；详情形态按 store 恢复 */
  async function init(): Promise<void> {
    if (initialized.value) {
      return
    }
    initialized.value = true
    try {
      await Promise.allSettled([loadList(), loadDefault()])
      if (store.pane === 'detail') {
        const persona = store.personas.find((item) => item.uid === store.activeUid)
        if (persona) {
          draft.value = draftOf(persona)
          void loadStatus(persona.uid, store.statusStateOf(persona.uid) === 'error')
        } else {
          store.backToList()
        }
      }
    } finally {
      booting.value = false
    }
  }

  // ===== 详情 =====

  function resetPrompt(): void {
    promptOpen.value = false
    promptTier.value = 'static'
    promptInput.value = ''
    promptStatic.value = null
    promptFull.value = null
    promptError.value = ''
    promptHint.value = ''
    promptLoading.value = false
    promptCopied.value = false
  }

  function openDetail(uid: string): void {
    const persona = store.personas.find((item) => item.uid === uid)
    if (!persona) {
      return
    }
    store.openDetail(uid)
    draft.value = draftOf(persona)
    saveError.value = ''
    resetPrompt()
    void loadStatus(uid, store.statusStateOf(uid) === 'error')
    // 折叠态就要有内容可读：进详情即取静态段，不等待用户展开
    void loadPrompt('static', null)
  }

  function doBackToList(): void {
    window.clearTimeout(statusTimer)
    store.backToList()
    draft.value = null
    saveError.value = ''
    resetPrompt()
  }

  /** 返回列表：有未保存修改先确认 */
  function requestBack(): void {
    if (dirty.value) {
      discardOpen.value = true
      return
    }
    doBackToList()
  }

  function confirmDiscard(): void {
    discardOpen.value = false
    doBackToList()
  }

  function setName(value: string): void {
    if (!draft.value) {
      return
    }
    draft.value = { ...draft.value, name: clampText(value, NAME_MAX) }
  }

  function setAvatar(value: string): void {
    if (!draft.value) {
      return
    }
    draft.value = { ...draft.value, avatar: value }
  }

  function setDescription(value: string): void {
    if (!draft.value) {
      return
    }
    draft.value = { ...draft.value, description: clampText(value, DESCRIPTION_MAX) }
  }

  async function save(): Promise<void> {
    const persona = active.value
    const current = draft.value
    if (!persona || !current || saving.value) {
      return
    }
    const invalid = nameError(current.name)
    if (invalid) {
      pushToast({ tone: 'warn', message: invalid })
      return
    }
    saving.value = true
    saveError.value = ''
    try {
      // 头像未改动时不提交：后端按「缺省保持旧值」处理，避免空串覆盖原路径
      const request: PersonaUpdateRequest = {
        name: current.name.trim(),
        description: current.description,
      }
      if (current.avatar.trim() !== draftOf(persona).avatar.trim()) {
        request.avatar = current.avatar.trim()
      }
      const updated = await personaApi.updateInfo(persona.uid, request)
      store.upsert(updated)
      // 对话页头部与列表共用同一人格缓存，保存后同步一次姓名 / 头像
      session.patchPersona(updated)
      draft.value = draftOf(updated)
      pushToast({ tone: 'success', message: '人格已保存' })
    } catch (failure) {
      saveError.value = errorMessage(failure, '保存人格失败')
      pushToast({ tone: 'danger', message: saveError.value })
    } finally {
      saving.value = false
    }
  }

  async function setDefault(): Promise<void> {
    const persona = active.value
    if (!persona || defaultSaving.value || isDefault.value) {
      return
    }
    defaultSaving.value = true
    try {
      await configApi.updateSetting('default_persona_uid', persona.uid)
      store.defaultUid = persona.uid
      store.defaultLoaded = true
      store.defaultError = ''
      pushToast({ tone: 'success', message: `已将「${persona.name}」设为默认对话人格` })
    } catch (failure) {
      pushToast({ tone: 'danger', message: errorMessage(failure, '设置默认人格失败') })
    } finally {
      defaultSaving.value = false
    }
  }

  // ===== 重载画像 =====

  function requestReload(): void {
    if (!active.value || reloadingProfile.value || llmUnavailable.value) {
      return
    }
    reloadOpen.value = true
  }

  function scheduleStatusRefresh(uid: string): void {
    window.clearTimeout(statusTimer)
    statusTimer = window.setTimeout(() => {
      store.clearStatus(uid)
      if (store.activeUid === uid) {
        void loadStatus(uid, true)
      }
    }, STATUS_REFRESH_DELAY_MS)
  }

  async function reloadProfile(): Promise<void> {
    const persona = active.value
    if (!persona || reloadingProfile.value) {
      return
    }
    reloadingProfile.value = true
    try {
      await personaApi.refresh(persona.uid)
      reloadOpen.value = false
      pushToast({ tone: 'success', message: '已提交重算，画像将在后台更新' })
      scheduleStatusRefresh(persona.uid)
    } catch (failure) {
      reloadOpen.value = false
      pushToast({ tone: 'danger', message: errorMessage(failure, '重载画像失败') })
    } finally {
      reloadingProfile.value = false
    }
  }

  // ===== 导出 =====

  function openExport(): void {
    exportError.value = ''
    exportOpen.value = true
  }

  /** 导出人格预设；取消保存或写入失败返回 false（错误由模态内呈现） */
  async function exportPersona(withExamples: boolean): Promise<boolean> {
    const persona = active.value
    if (!persona) {
      return false
    }
    const path = await systemApi.saveFileDialog(`ramaria-persona-${persona.uid}.toml`, '人格预设')
    if (!path) {
      return false
    }
    const receipt = await personaApi.personaExport(persona.uid, withExamples, path)
    pushToast({ tone: 'success', message: exportReceiptText(receipt) })
    if (receipt.hints && receipt.hints.length > 0) {
      pushToast({ tone: 'warn', message: receipt.hints.join('；') })
    }
    return true
  }

  async function confirmExport(withExamples: boolean): Promise<void> {
    if (exporting.value) {
      return
    }
    exporting.value = true
    exportError.value = ''
    try {
      const done = await exportPersona(withExamples)
      if (done) {
        exportOpen.value = false
      }
    } catch (failure) {
      exportError.value = errorMessage(failure, '导出人格失败')
    } finally {
      exporting.value = false
    }
  }

  function openImport(): void {
    importOpen.value = true
  }

  // ===== 提示词预览 =====

  /** 当前可见的提示词正文：折叠态恒为静态段 */
  const promptText = computed(() => {
    const hit = promptOpen.value && promptTier.value === 'full' ? promptFull.value : promptStatic.value
    return hit?.system_prompt ?? ''
  })

  /**
   * 读取指定档位提示词。
   *
   * 参数:
   * - `tier`: `static` 静态段（无召回）；`full` 全量（需示例输入）。
   * - `input`: 全量档的示例输入；静态档传 `null`。
   */
  async function loadPrompt(tier: PromptTier, input: string | null): Promise<void> {
    const persona = active.value
    if (!persona) {
      return
    }
    promptLoading.value = true
    promptError.value = ''
    promptCopied.value = false
    try {
      const preview = await personaApi.previewPrompt(persona.uid, input)
      if (tier === 'full') {
        promptFull.value = preview
      } else {
        promptStatic.value = preview
      }
    } catch (failure) {
      if (tier === 'full') {
        promptFull.value = null
      } else {
        promptStatic.value = null
      }
      const reason = errorMessage(failure, '')
      promptError.value = reason === '' ? '提示词读取失败' : `提示词读取失败：${reason}`
    } finally {
      promptLoading.value = false
    }
  }

  /** 读取当前档位：静态段直接重取，全量档需示例输入 */
  function requestPrompt(): void {
    if (promptTier.value === 'static') {
      void loadPrompt('static', null)
      return
    }
    const input = promptInput.value.trim()
    if (input.length === 0) {
      promptHint.value = '输入一句示例文本后再生成全量预览'
      return
    }
    promptHint.value = ''
    void loadPrompt('full', input)
  }

  /** 展开 / 收起：展开即读静态段，收起不改档位 */
  function togglePrompt(): void {
    if (promptOpen.value) {
      promptOpen.value = false
      promptTier.value = 'static'
      promptCopied.value = false
      return
    }
    promptOpen.value = true
    if (promptStatic.value === null && !promptLoading.value) {
      void loadPrompt('static', null)
    }
  }

  /** 静态段 / 编译预览切换；回到静态段视为收起编译态 */
  function setPromptTier(tier: PromptTier): void {
    if (promptTier.value === tier) {
      return
    }
    promptTier.value = tier
    promptError.value = ''
    promptHint.value = ''
    promptCopied.value = false
    if (tier === 'static') {
      promptInput.value = ''
      return
    }
    if (promptInput.value.trim().length > 0) {
      void loadPrompt('full', promptInput.value.trim())
    }
  }

  function setPromptInput(value: string): void {
    promptInput.value = value
    if (promptHint.value !== '') {
      promptHint.value = ''
    }
  }

  async function copyPrompt(): Promise<void> {
    const text = promptText.value
    if (text.length === 0) {
      return
    }
    try {
      if (!navigator.clipboard || !navigator.clipboard.writeText) {
        throw new Error('clipboard unavailable')
      }
      await navigator.clipboard.writeText(text)
      promptCopied.value = true
      pushToast({ tone: 'success', message: '已复制提示词' })
    } catch {
      pushToast({ tone: 'warn', message: '复制失败，请手动选择文本' })
    }
  }

  // ===== 导入 =====

  /** 导入完成：提示结果、刷新列表并进入新人格详情 */
  async function onImported(outcome: PresetImportOutcome): Promise<void> {
    importOpen.value = false
    pushToast({
      tone: outcome.written.failed > 0 ? 'warn' : 'success',
      message: importOutcomeText(outcome),
    })
    await loadList()
    if (store.personas.some((item) => item.uid === outcome.uid)) {
      openDetail(outcome.uid)
    }
  }

  async function retry(): Promise<void> {
    store.listError = ''
    if (store.defaultError !== '') {
      store.defaultLoaded = false
    }
    await Promise.allSettled([loadList(), loadDefault()])
  }

  onBeforeUnmount(() => {
    window.clearTimeout(statusTimer)
  })

  return {
    store,
    booting,
    saving,
    saveError,
    defaultSaving,
    draft,
    discardOpen,
    reloadOpen,
    reloadingProfile,
    exportOpen,
    exporting,
    exportError,
    importOpen,
    promptOpen,
    promptTier,
    promptInput,
    promptStatic,
    promptFull,
    promptText,
    promptLoading,
    promptError,
    promptHint,
    promptCopied,
    active,
    isDefault,
    defaultCount,
    viewState,
    busy,
    partialError,
    dirty,
    canSave,
    llmUnavailable,
    profileState,
    profile,
    profileDegraded,
    profileChip,
    profileNote,
    init,
    openDetail,
    requestBack,
    confirmDiscard,
    setName,
    setAvatar,
    setDescription,
    save,
    setDefault,
    requestReload,
    reloadProfile,
    openExport,
    confirmExport,
    openImport,
    requestPrompt,
    togglePrompt,
    setPromptTier,
    setPromptInput,
    copyPrompt,
    onImported,
    retry,
  }
}
