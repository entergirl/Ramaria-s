/**
 * src/views/import/useImportView.ts — 导入视图的数据与动作编排
 *
 * 职责:
 * - 四步向导（选择 / 预览 / 执行 / 完成）的状态推进与错误收束
 * - 文件选择、解析、导入、继续处理、续跑状态查询
 * - 从常驻任务状态（app store）派生进度、阶段与完成统计
 *
 * 设计特点:
 * - 数据与动作集中在此，组件只做布局与事件接线
 * - 任务进度取自 app store 的常驻订阅：离开视图不中断，回来仍在执行则回到执行步
 * - 错误分两处：解析失败停在预览步，导入 / 续跑失败停在执行步，均提供重试
 * - 完成页动作只经一次性跳转参数进入其他视图，不直读对方 store 内部字段
 */

import { computed, onMounted, onUnmounted, reactive, ref, watch } from 'vue'
import { useRouter } from 'vue-router'

import { importApi, systemApi } from '../../core/api'
import type { ImportAnalysisReport, ImportResult, ImportStatusView } from '../../core/api/dto'
import { errorMessage } from '../../core/errors'
import {
  applyReportDefaults,
  defaultImportForm,
  fileDisplayName,
  formError,
  submitPayload,
  type ImportForm,
} from '../../core/import/form'
import {
  continueMetrics,
  doneMetrics,
  progressView,
  stageState,
  stagesForMode,
  type ImportMetric,
} from '../../core/import/progress'
import {
  memberText,
  modeLabel,
  reportMetrics,
  resultMetrics,
  timeRangeText,
} from '../../core/import/report'
import { pushToast } from '../../core/toast'
import { useNow } from '../../core/useNow'
import { useAppStore } from '../../stores/app'
import { usePersonaStore } from '../../stores/persona'

/** 向导步骤 */
export type ImportStep = 'select' | 'preview' | 'run' | 'done'

/** 视图主态（与 design-spec §9.2 对齐） */
export type ImportViewState = 'default' | 'loading' | 'empty' | 'error'

/** 步骤条标签 */
export const IMPORT_STEP_LABELS = ['选择文件', '预览报告', '执行导入', '完成'] as const

/** 步骤序号（1~4） */
export function stepNumber(step: ImportStep): number {
  switch (step) {
    case 'select':
      return 1
    case 'preview':
      return 2
    case 'run':
      return 3
    default:
      return 4
  }
}

export function useImportView() {
  const router = useRouter()
  const app = useAppStore()
  const personaStore = usePersonaStore()

  const step = ref<ImportStep>('select')
  const form = reactive<ImportForm>(defaultImportForm())
  const report = ref<ImportAnalysisReport | null>(null)
  const result = ref<ImportResult | null>(null)
  const analyzing = ref(false)
  const submitting = ref(false)
  const error = ref('')
  const resumeStatus = ref<ImportStatusView | null>(null)
  const statusLoading = ref(false)
  const statusError = ref('')
  /** 未选导出目录时是否等待用户确认「仅补摘要」 */
  const pendingContinue = ref(false)
  const now = useNow(1000)

  const job = computed(() => app.importJob)
  const running = computed(() => job.value?.running === true)
  const jobFailed = computed(() => job.value?.failed === true)
  const continueJob = computed(() => job.value?.kind === 'continue')
  const busy = computed(() => analyzing.value || submitting.value || running.value)

  // =========================================================
  // 派生：主态 / 受限 / 降级 / 步骤
  // =========================================================

  const viewState = computed<ImportViewState>(() => {
    if (step.value === 'select') {
      if (error.value) {
        return 'error'
      }
      return form.filePath ? 'default' : 'empty'
    }
    if (step.value === 'preview') {
      if (analyzing.value) {
        return 'loading'
      }
      return error.value ? 'error' : 'default'
    }
    if (step.value === 'run') {
      return jobFailed.value ? 'error' : 'loading'
    }
    return 'default'
  })

  const restricted = computed<'locked' | undefined>(() => (running.value ? 'locked' : undefined))

  const l1Failed = computed(() => job.value?.payload?.l1_failed ?? 0)
  const l1Success = computed(() => job.value?.payload?.l1_success ?? 0)
  const visionFailed = computed(() => job.value?.payload?.vision_failed ?? 0)
  const skippedAttachments = computed(() => resumeStatus.value?.skipped_attachments ?? 0)

  /** L1 失败、图片理解失败或存量附件被跳过：完成页走降级提示 */
  const degraded = computed<'llm' | undefined>(() =>
    step.value === 'done' &&
    (l1Failed.value > 0 || visionFailed.value > 0 || skippedAttachments.value > 0)
      ? 'llm'
      : undefined,
  )

  const stages = computed(() =>
    stagesForMode(continueJob.value ? 'deep' : result.value?.mode ?? form.mode),
  )

  const progress = computed(() => {
    const payload = job.value?.payload
    if (!payload || !job.value) {
      return null
    }
    return progressView(payload, job.value.startedAt, now.value)
  })

  /** 阶段条：按当前进度补上格位状态 */
  const stageViews = computed(() =>
    stages.value.map((stage) => ({
      ...stage,
      state: stageState(job.value?.phase ?? '', stage.key),
    })),
  )

  // =========================================================
  // 派生：文案与统计
  // =========================================================

  const fileName = computed(() => fileDisplayName(form.filePath))
  const modeText = computed(() => modeLabel(result.value?.mode ?? form.mode))
  const reportStats = computed<ImportMetric[]>(() =>
    report.value ? reportMetrics(report.value) : [],
  )
  const resultStats = computed<ImportMetric[]>(() =>
    result.value ? resultMetrics(result.value) : [],
  )
  const doneStats = computed<ImportMetric[]>(() => doneMetrics(job.value?.payload ?? null))
  const continueStats = computed<ImportMetric[]>(() =>
    continueMetrics(job.value?.payload ?? null),
  )
  const reportMembers = computed(() =>
    report.value ? memberText(report.value.members, 5) : '',
  )
  const resultMembers = computed(() =>
    result.value ? memberText(result.value.members, 3) : '',
  )
  const timeRange = computed(() =>
    timeRangeText(report.value?.time_range ?? result.value?.time_range ?? ''),
  )
  const l1Total = computed(() => l1Success.value + l1Failed.value)

  const resumable = computed(() => resumeStatus.value?.resumable === true && !running.value)
  const resumeSummary = computed(() => {
    const status = resumeStatus.value
    if (!status) {
      return ''
    }
    const parts: string[] = []
    if (status.pending_attachments > 0) {
      parts.push(`${status.pending_attachments} 个附件待处理`)
    }
    if (status.skipped_attachments > 0) {
      parts.push(`${status.skipped_attachments} 个附件曾被跳过`)
    }
    if (status.missing_l1_sessions > 0) {
      parts.push(`${status.missing_l1_sessions} 个会话缺摘要`)
    }
    return parts.join(' · ')
  })

  // =========================================================
  // 续跑状态
  // =========================================================

  async function loadResumeStatus(): Promise<void> {
    statusLoading.value = true
    statusError.value = ''
    try {
      resumeStatus.value = await importApi.getStatus()
    } catch (failure) {
      statusError.value = errorMessage(failure, '续跑状态读取失败')
    } finally {
      statusLoading.value = false
    }
  }

  // =========================================================
  // 步骤一：选择文件
  // =========================================================

  async function selectFile(): Promise<void> {
    if (busy.value) {
      return
    }
    try {
      const picked = await systemApi.openFileDialog({
        title: '选择 QQ 聊天记录文件',
        filterName: 'QQ 聊天记录',
        extensions: ['json'],
      })
      if (!picked) {
        return
      }
      form.filePath = picked
      report.value = null
      result.value = null
      error.value = ''
    } catch (failure) {
      error.value = errorMessage(failure, '打开文件对话框失败')
    }
  }

  /** 更换文件：只清文件与报告，保留模式与 persona 填写 */
  function changeFile(): void {
    form.filePath = ''
    report.value = null
    error.value = ''
    step.value = 'select'
  }

  // =========================================================
  // 步骤二：解析预览
  // =========================================================

  async function analyze(): Promise<void> {
    if (busy.value) {
      return
    }
    const message = formError(form)
    if (message) {
      error.value = message
      return
    }
    analyzing.value = true
    error.value = ''
    report.value = null
    step.value = 'preview'
    try {
      const parsed = await importApi.analyzeFile(form.filePath, form.gapMinutes)
      report.value = parsed
      Object.assign(form, applyReportDefaults(form, parsed))
    } catch (failure) {
      error.value = errorMessage(failure, '文件解析失败')
    } finally {
      analyzing.value = false
    }
  }

  function backToSelect(): void {
    step.value = 'select'
    error.value = ''
  }

  // =========================================================
  // 步骤三：执行导入
  // =========================================================

  async function confirmImport(): Promise<void> {
    if (busy.value || !report.value) {
      return
    }
    const payload = submitPayload(form)
    error.value = ''
    result.value = null
    app.clearImportJob()
    app.startImportJob('import')
    submitting.value = true
    step.value = 'run'

    try {
      result.value = await importApi.importQQ(
        form.filePath,
        payload.mode,
        payload.personaName,
        payload.selfPersonaUid,
        payload.otherPersonaName,
        payload.otherPersonaUid,
        payload.gapMinutes,
        payload.side,
      )
    } catch (failure) {
      const message = errorMessage(failure, '导入失败')
      error.value = message
      app.failImportJob(message)
    } finally {
      submitting.value = false
    }
  }

  /** 执行步重试：导入重发 / 续跑重启 */
  async function retryRun(): Promise<void> {
    if (continueJob.value) {
      await continueImport()
      return
    }
    error.value = ''
    await confirmImport()
  }

  // =========================================================
  // 继续处理 / 续跑
  // =========================================================

  /** 发起续跑（`exportRoot = null` 时跳过附件回补） */
  async function runContinue(exportRoot: string | null): Promise<void> {
    error.value = ''
    result.value = null
    app.clearImportJob()
    app.startImportJob('continue')
    step.value = 'run'
    try {
      await importApi.continueImport(exportRoot)
    } catch (failure) {
      const message = errorMessage(failure, '继续处理失败')
      error.value = message
      app.failImportJob(message)
    }
  }

  async function continueImport(): Promise<void> {
    if (busy.value) {
      return
    }
    let dir: string | null = null
    try {
      dir = await systemApi.openDirectoryDialog(
        null,
        '选择 QQ 聊天记录导出目录（含 resources/ 的那一层）',
      )
    } catch (failure) {
      pushToast({
        tone: 'danger',
        message: errorMessage(failure, '打开目录对话框失败'),
      })
      return
    }
    if (!dir) {
      // 取消目录选择：仍可补摘要与画像，由用户二次确认
      pendingContinue.value = true
      return
    }
    await runContinue(dir)
  }

  /** 仅补摘要与画像（用户确认不选目录） */
  async function continueWithoutDir(): Promise<void> {
    pendingContinue.value = false
    await runContinue(null)
  }

  // =========================================================
  // 步骤四：完成动作
  // =========================================================

  /** 导入另一个文件：清空表单与任务，回到选择步 */
  function importAnother(): void {
    Object.assign(form, defaultImportForm())
    report.value = null
    result.value = null
    error.value = ''
    app.clearImportJob()
    step.value = 'select'
    void loadResumeStatus()
  }

  /** 前往记忆：优先导出者人格，缺失时回退对方 */
  function goMemory(): void {
    const uid = result.value?.persona_uid ?? result.value?.other_persona_uid ?? null
    if (uid) {
      personaStore.requestPreselect(uid)
    }
    void router.push({ name: 'memory' })
  }

  /** 查看导入消息：打开最后一个导入会话（只读与导入形态由会话数据派生） */
  function goChat(): void {
    const sessionId = result.value?.session_ids?.at(-1)
    if (sessionId) {
      void router.push({ name: 'chat', query: { sessionId } })
      return
    }
    void router.push({ name: 'chat' })
  }

  // =========================================================
  // 生命周期
  // =========================================================

  watch(
    () => app.importJob?.finishedAt ?? null,
    (finishedAt) => {
      if (!finishedAt || step.value !== 'run') {
        return
      }
      if (app.importJob?.failed) {
        return
      }
      step.value = 'done'
      void loadResumeStatus()
    },
  )

  onMounted(() => {
    app.setImportViewAttached(true)
    void loadResumeStatus()
    if (app.importJob?.running && !app.importJob.failed) {
      step.value = 'run'
    }
  })

  onUnmounted(() => {
    app.setImportViewAttached(false)
  })

  return {
    // 状态
    step,
    form,
    report,
    result,
    analyzing,
    submitting,
    error,
    resumeStatus,
    statusLoading,
    statusError,
    pendingContinue,
    now,
    job,
    // 派生
    busy,
    running,
    jobFailed,
    continueJob,
    viewState,
    restricted,
    degraded,
    stages,
    progress,
    stageViews,
    fileName,
    modeText,
    reportStats,
    resultStats,
    doneStats,
    continueStats,
    reportMembers,
    resultMembers,
    timeRange,
    l1Failed,
    l1Success,
    l1Total,
    visionFailed,
    skippedAttachments,
    resumable,
    resumeSummary,
    // 动作
    selectFile,
    changeFile,
    analyze,
    backToSelect,
    confirmImport,
    retryRun,
    continueImport,
    continueWithoutDir,
    importAnother,
    goMemory,
    goChat,
    loadResumeStatus,
  }
}
