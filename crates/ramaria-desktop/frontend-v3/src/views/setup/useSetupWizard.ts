/**
 * src/views/setup/useSetupWizard.ts — 首次配置向导的状态与动作
 *
 * 职责:
 * - 承载三步向导的表单状态、连接测试、嵌入校验、隐私确认与提交动作
 *
 * 设计特点:
 * - 视图只负责布局与提示，异步动作与状态迁移集中在本组合式函数
 * - 提交顺序固定为「嵌入模型 → 首次配置 → 隐私确认 → 图片理解声明」，任一步失败不吞错
 * - 非关键步骤（嵌入保存 / 图片理解声明）失败只记警告，不阻断进入应用
 */

import { computed, reactive, ref } from 'vue'

import { chatApi, configApi, setupApi } from '../../core/api'
import type { BackendConfig, EmbeddingValidation } from '../../core/api/dto'
import { errorMessage } from '../../core/errors'
import {
  collectSetupRequest,
  hasFieldErrors,
  initialFields,
  normalizeModelPath,
  parseConfigJson,
  readVisionSupport,
  SETUP_COMPLETED_KEY,
  validateEmbeddingPath,
  validateLlmFields,
  withVisionSupport,
  type SetupFieldErrors,
  type SetupFields,
  type SetupMode,
} from '../../core/setupWizard'
import { pushToast } from '../../core/toast'
import { useAppStore } from '../../stores/app'

/** 连接测试的三态结果 */
export type ConnectionState = 'idle' | 'ok' | 'fail'

export function useSetupWizard() {
  const app = useAppStore()

  const step = ref(1)
  const fields = reactive<SetupFields>(initialFields())
  const errors = ref<SetupFieldErrors>({})

  const testing = ref(false)
  const connectionState = ref<ConnectionState>('idle')
  const connectionMessage = ref('')

  const validating = ref(false)
  const embeddingValidation = ref<EmbeddingValidation | null>(null)
  const embeddingSkipped = ref(false)

  const privacyChoice = reactive({ accepted: false, persistent: true })
  const privacyLoading = ref(false)
  /** 已确认过的「运行方式 + 地址」键：来回切步骤时不重复要求确认 */
  let acceptedKey = ''

  const submitting = ref(false)
  const submitError = ref('')
  const entering = ref(false)

  const embeddingReady = computed(() => embeddingValidation.value?.valid === true)

  /** 线上后端是否还需用户确认隐私范围（未查询到状态时按需确认处理） */
  const privacyNeedsConfirm = computed(
    () => fields.mode === 'online' && !privacyChoice.accepted,
  )

  /** 隐私确认的归属键（运行方式 + 地址） */
  function privacyKey(): string {
    return `${fields.mode}|${fields.apiUrl.trim()}`
  }

  /** 记录本次会话内的隐私确认 */
  function acceptPrivacy(persistent: boolean): void {
    privacyChoice.accepted = true
    privacyChoice.persistent = persistent
    acceptedKey = privacyKey()
  }

  /** 回显已保存的后端配置，避免重跑向导时把既有 provider 覆盖成默认值 */
  function applyBackend(backend: BackendConfig): void {
    const provider = backend.provider.trim().toLowerCase()
    const isLocal = provider.includes('lmstudio') || provider.includes('lm_studio')
    if (isLocal) {
      fields.mode = 'local'
      if (backend.baseUrl.trim()) {
        fields.localUrl = backend.baseUrl.trim()
      }
      if (backend.modelId.trim()) {
        fields.localModel = backend.modelId.trim()
      }
      return
    }
    if (provider === 'deepseek' || provider === 'openai') {
      fields.mode = 'online'
      if (backend.baseUrl.trim()) {
        fields.apiUrl = backend.baseUrl.trim()
      }
      if (backend.modelId.trim()) {
        fields.apiModel = backend.modelId.trim()
      }
    }
  }

  /** 读取已保存的后端配置、嵌入模型与图片理解声明（失败不阻塞向导） */
  async function loadInitialState(): Promise<void> {
    const [embeddingResult, configResult, backendResult] = await Promise.allSettled([
      setupApi.getEmbeddingModel(),
      configApi.getFull(),
      configApi.getBackend(),
    ])

    if (embeddingResult.status === 'fulfilled' && embeddingResult.value) {
      const model = embeddingResult.value
      if (model.model_path) {
        fields.embeddingPath = model.model_path
      }
      if (model.valid) {
        embeddingValidation.value = { valid: true, dimension: model.dimension }
      }
    }

    if (configResult.status === 'fulfilled') {
      fields.visionSupported = readVisionSupport(parseConfigJson(configResult.value))
    }

    if (backendResult.status === 'fulfilled') {
      applyBackend(backendResult.value)
    }
  }

  /** 切换运行方式：连接测试与隐私确认随之失效 */
  function setMode(mode: SetupMode): void {
    if (fields.mode === mode) {
      return
    }
    fields.mode = mode
    connectionState.value = 'idle'
    connectionMessage.value = ''
    privacyChoice.accepted = false
    acceptedKey = ''
    errors.value = {}
  }

  /** 校验对话模型字段；返回字段错误数 */
  function validateModelStep(): number {
    errors.value = validateLlmFields(fields)
    return hasFieldErrors(errors.value) ? 1 : 0
  }

  /** 保存后端配置并测试连接 */
  async function testConnection(): Promise<boolean> {
    if (validateModelStep() > 0) {
      return false
    }
    const request = collectSetupRequest(fields)
    testing.value = true
    connectionState.value = 'idle'
    connectionMessage.value = ''
    try {
      await configApi.updateBackend(
        request.provider,
        request.modelId,
        request.baseUrl,
        request.apiKey,
      )
      await setupApi.testLlmConnection()
      connectionState.value = 'ok'
      connectionMessage.value =
        fields.mode === 'local' ? '本地推理服务可达' : '线上 API 可达'
      return true
    } catch (error) {
      connectionState.value = 'fail'
      connectionMessage.value = errorMessage(error, '连接测试失败')
      return false
    } finally {
      testing.value = false
    }
  }

  /** 校验嵌入模型路径 */
  async function validateEmbedding(): Promise<boolean> {
    const path = normalizeModelPath(fields.embeddingPath)
    fields.embeddingPath = path
    const message = validateEmbeddingPath(path)
    errors.value = { ...errors.value, embeddingPath: message }
    if (message) {
      return false
    }

    validating.value = true
    embeddingValidation.value = null
    try {
      const result = await setupApi.validateEmbeddingModel(path)
      embeddingValidation.value = result
      if (result.valid) {
        embeddingSkipped.value = false
        errors.value = { ...errors.value, embeddingPath: undefined }
      }
      return result.valid
    } catch (error) {
      embeddingValidation.value = {
        valid: false,
        reason: errorMessage(error, '校验嵌入模型失败'),
      }
      return false
    } finally {
      validating.value = false
    }
  }

  /** 跳过嵌入模型（进入降级模式） */
  function skipEmbedding(): void {
    embeddingSkipped.value = true
    embeddingValidation.value = null
    errors.value = { ...errors.value, embeddingPath: undefined }
  }

  /** 进入确认步骤时查询隐私确认状态（仅在本次已测试连接、后端配置已更新时查询） */
  async function loadPrivacyStatus(): Promise<void> {
    if (fields.mode !== 'online' || connectionState.value !== 'ok') {
      privacyChoice.accepted = false
      return
    }

    privacyLoading.value = true
    try {
      const status = await chatApi.checkPrivacy()
      if (status.status === 'Confirmed') {
        privacyChoice.accepted = true
        privacyChoice.persistent = status.persistent
        acceptedKey = privacyKey()
      } else {
        // 同一「运行方式 + 地址」在本次会话已确认过，不重复要求
        privacyChoice.accepted = acceptedKey === privacyKey()
      }
    } catch {
      privacyChoice.accepted = acceptedKey === privacyKey()
    } finally {
      privacyLoading.value = false
    }
  }

  /**
   * 写入图片理解声明。
   *
   * 说明:
   * - 必须先读最新配置再改：本次 `run_setup` 刚写过 `[backend]` 组，
   *   使用进入向导时的旧快照会把后端配置回退成保存前的值
   * - 只改一个键，其余键原样写回；双侧写入失败以回执首条原因返回
   */
  async function saveVisionSupport(): Promise<string> {
    const parsed = parseConfigJson(await configApi.getFull())
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      throw new Error('配置结构无法识别')
    }

    const next = withVisionSupport(parsed, fields.visionSupported)
    if (!next) {
      throw new Error('配置结构无法识别')
    }
    const receipt = await configApi.updateFull(JSON.stringify(next))
    return receipt.failures[0] ?? ''
  }

  /**
   * 提交首次配置。
   *
   * 返回:
   * - `true`: 已写入并完成首次配置；`false`: 被前置校验或写入失败拦下。
   */
  async function submit(): Promise<boolean> {
    if (privacyNeedsConfirm.value && !privacyChoice.accepted) {
      submitError.value = '请先确认线上后端的数据范围与去向'
      return false
    }

    submitting.value = true
    submitError.value = ''
    const warnings: string[] = []

    try {
      if (embeddingReady.value && !embeddingSkipped.value) {
        try {
          await setupApi.saveEmbeddingModel(normalizeModelPath(fields.embeddingPath))
        } catch (error) {
          warnings.push(errorMessage(error, '嵌入模型保存失败'))
        }
      }

      const request = collectSetupRequest(fields)
      await setupApi.run(request.provider, request.modelId, request.baseUrl, request.apiKey)

      if (fields.mode === 'online') {
        try {
          await chatApi.confirmPrivacy(privacyChoice.persistent)
        } catch (error) {
          warnings.push(errorMessage(error, '隐私确认记录失败'))
        }
      }

      try {
        const failure = await saveVisionSupport()
        if (failure) {
          warnings.push(failure)
        }
      } catch (error) {
        warnings.push(errorMessage(error, '图片理解声明保存失败'))
      }

      // 完成标记：冷启动据此区分「走完向导」与「只点过测试连接」（服务层落点见 M4-6）
      try {
        await configApi.updateSetting(SETUP_COMPLETED_KEY, String(Date.now()))
      } catch (error) {
        warnings.push(errorMessage(error, '首次配置完成标记写入失败'))
      }

      const firstWarning = warnings[0]
      if (firstWarning) {
        pushToast({ tone: 'warn', message: firstWarning })
      }
      return true
    } catch (error) {
      submitError.value = errorMessage(error, '配置保存失败')
      pushToast({ tone: 'danger', message: submitError.value })
      return false
    } finally {
      submitting.value = false
    }
  }

  /** 进入主界面：刷新应用与索引状态，路由跟随状态迁移 */
  async function enterApp(): Promise<void> {
    entering.value = true
    try {
      await app.refreshAppState()
      await app.refreshIndex()
      if (app.appState === 'needs_setup') {
        pushToast({ tone: 'danger', message: '状态刷新未通过，请检查后端配置' })
      }
    } finally {
      entering.value = false
    }
  }

  return {
    app,
    step,
    fields,
    errors,
    testing,
    connectionState,
    connectionMessage,
    validating,
    embeddingValidation,
    embeddingReady,
    embeddingSkipped,
    privacyChoice,
    privacyLoading,
    submitting,
    submitError,
    entering,
    loadInitialState,
    setMode,
    validateModelStep,
    testConnection,
    validateEmbedding,
    skipEmbedding,
    loadPrivacyStatus,
    acceptPrivacy,
    submit,
    enterApp,
  }
}
