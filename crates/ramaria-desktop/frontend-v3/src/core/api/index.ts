/**
 * src/core/api/index.ts — 按域的命令门面
 *
 * 职责:
 * - 把命令表映射为可调用的域方法：参数命名对齐 Rust 侧 camelCase，返回类型取 DTO
 *
 * 设计特点:
 * - 只做「方法 → 命令 + 参数形状」的转写，不含业务判断与状态
 * - 域方法名与 `commands.ts` 的方法名一一对应，便于按域检索调用点
 * - 可选参数统一显式传 `null`，与 Rust `Option<T>` 语义一致
 * - 当前只覆盖 M5 消费的域；其余域随视图落地增量补齐
 */

import { invoke } from '../transport'
import { COMMANDS } from './commands'
import type {
  AppStateValue,
  BackendConfig,
  BehaviorRuleView,
  ChannelBinding,
  ChannelPanel,
  ClearCacheResult,
  ConfigWriteReceipt,
  DegradedReason,
  DiagnosticsExport,
  EmbeddingModelView,
  EmbeddingValidation,
  FactList,
  ImportAnalysisReport,
  ImportPipelineReceipt,
  ImportResult,
  ImportStatusView,
  IndexRebuildReceipt,
  IndexStatusView,
  McpInfo,
  MemoryEvent,
  MemoryL1,
  PersonaExportReceipt,
  PersonaFull,
  PersonaPromptPreview,
  PersonaSummary,
  PersonaUpdateRequest,
  PersonalityProfile,
  PresetImportOutcome,
  PresetImportPreview,
  PrivacyStatus,
  ProactiveOverview,
  ProactivePersonaRow,
  ProfileStatus,
  RuleEnabledReceipt,
  RuleEvidenceResponse,
  RuleListResponse,
  SendLocalImageResult,
  SessionDetail,
  SessionSummary,
  SettingItem,
  TraitEvidenceChain,
  UpdateStatus,
  VisionSupportReceipt,
} from './dto'

/** 对话：发送、状态、隐私、撤回、本地发图 */
export const chatApi = {
  send(message: string, personaUid: string | null = null, sessionId: string | null = null) {
    return invoke<string>(COMMANDS.chat.send, { message, personaUid, sessionId })
  },
  /** 保存对话：关闭活跃会话并触发 L1；返回 JSON 文本（形状见 `SaveSessionReceipt`） */
  save(personaUid: string | null = null) {
    return invoke<string>(COMMANDS.chat.save, { personaUid })
  },
  /** 重试 L1 摘要；返回 JSON 文本（形状见 `L1RetryReceipt`） */
  generateL1(sessionId: string, personaUid: string | null = null) {
    return invoke<string>(COMMANDS.chat.generateL1, { sessionId, personaUid })
  },
  getAppState() {
    return invoke<AppStateValue>(COMMANDS.chat.getAppState)
  },
  checkPrivacy() {
    return invoke<PrivacyStatus>(COMMANDS.chat.checkPrivacy)
  },
  confirmPrivacy(persistent: boolean) {
    return invoke<string>(COMMANDS.chat.confirmPrivacy, { persistent })
  },
  recallMessage(messageId: string) {
    return invoke<null>(COMMANDS.chat.recallMessage, { messageId })
  },
  sendLocalImage(sessionId: string, filePath: string, caption: string | null = null) {
    return invoke<SendLocalImageResult>(COMMANDS.chat.sendLocalImage, {
      sessionId,
      filePath,
      caption,
    })
  },
}

/** 首次配置：执行配置、连接测试、嵌入模型与状态刷新 */
export const setupApi = {
  run(provider: string, modelId: string, baseUrl: string, apiKey: string | null = null) {
    return invoke<string>(COMMANDS.setup.run, { provider, modelId, baseUrl, apiKey })
  },
  refresh() {
    return invoke<AppStateValue>(COMMANDS.setup.refresh)
  },
  testLlmConnection() {
    return invoke<string>(COMMANDS.setup.testLlmConnection)
  },
  validateEmbeddingModel(path: string) {
    return invoke<EmbeddingValidation>(COMMANDS.setup.validateEmbeddingModel, { path })
  },
  saveEmbeddingModel(path: string) {
    return invoke<string>(COMMANDS.setup.saveEmbeddingModel, { path })
  },
  getEmbeddingModel() {
    return invoke<EmbeddingModelView | null>(COMMANDS.setup.getEmbeddingModel)
  },
  getDegradedReason() {
    return invoke<DegradedReason | null>(COMMANDS.setup.getDegradedReason)
  },
  /** 写入图片理解声明（只改 `[vision].model_supports_vision`） */
  setVisionSupport(enabled: boolean) {
    return invoke<VisionSupportReceipt>(COMMANDS.setup.setVisionSupport, { enabled })
  },
  /** 下载嵌入模型；进度经 `model-download-progress` 事件推送 */
  downloadEmbeddingModel(modelId: string) {
    return invoke<string>(COMMANDS.setup.downloadEmbeddingModel, { modelId })
  },
}

/** 会话：列表、详情、新建、已读 */
export const sessionApi = {
  list() {
    return invoke<SessionSummary[]>(COMMANDS.session.list)
  },
  get(sessionId: string, limit: number | null = null, offset: number | null = null) {
    return invoke<SessionDetail>(COMMANDS.session.get, { sessionId, limit, offset })
  },
  create(personaUid: string | null = null) {
    return invoke<SessionSummary>(COMMANDS.session.create, { personaUid })
  },
  markRead(sessionId: string) {
    return invoke<string>(COMMANDS.session.markRead, { sessionId })
  },
}

/** 配置：后端配置读写、全局设置读写、全量配置读写 */
export const configApi = {
  getBackend() {
    return invoke<BackendConfig>(COMMANDS.config.getBackend)
  },
  updateBackend(
    provider: string,
    modelId: string,
    baseUrl: string,
    apiKey: string | null = null,
  ) {
    return invoke<string>(COMMANDS.config.updateBackend, { provider, modelId, baseUrl, apiKey })
  },
  getSettings() {
    return invoke<SettingItem[]>(COMMANDS.config.getSettings)
  },
  updateSetting(key: string, value: string) {
    return invoke<string>(COMMANDS.config.updateSetting, { key, value })
  },
  /** 读取完整生效配置：命令返回 JSON 文本，由调用方解析（形状见 `[vision]` 等配置组） */
  getFull() {
    return invoke<string>(COMMANDS.config.getFull)
  },
  updateFull(configJson: string) {
    return invoke<ConfigWriteReceipt>(COMMANDS.config.updateFull, { configJson })
  },
  /** 出厂默认配置（JSON 文本）：设置页「恢复默认」的唯一来源 */
  getDefaults() {
    return invoke<string>(COMMANDS.config.getDefaults)
  },
}

/** 索引：状态与重建 */
export const indexApi = {
  getStatus() {
    return invoke<IndexStatusView>(COMMANDS.index.getStatus)
  },
  rebuild() {
    return invoke<IndexRebuildReceipt>(COMMANDS.index.rebuild)
  },
}

/** 附件：图片字节与缓存清理 */
export const attachmentApi = {
  readImage(messageId: string, index: number) {
    return invoke<ArrayBuffer>(COMMANDS.attachment.readImage, { messageId, index })
  },
  clearCache() {
    return invoke<ClearCacheResult>(COMMANDS.attachment.clearCache)
  },
}

/** 记忆：人格筛选、四页签取数、证据链与深度处理 */
export const memoryApi = {
  getPersonas() {
    return invoke<PersonaSummary[]>(COMMANDS.memory.getPersonas)
  },
  /** L1 摘要；`limit` 缺省时后端按 200 处理，上限 1000 */
  getL1(personaUid: string | null = null, limit: number | null = 500) {
    return invoke<MemoryL1[]>(COMMANDS.memory.getL1, { personaUid, limit })
  },
  /** L2 事件；`limit` 缺省时后端按 200 处理，上限 1000 */
  getL2(personaUid: string | null = null, limit: number | null = 500) {
    return invoke<MemoryEvent[]>(COMMANDS.memory.getL2, { personaUid, limit })
  },
  getProfile(personaUid: string) {
    return invoke<PersonalityProfile>(COMMANDS.memory.getProfile, { personaUid })
  },
  getProfileStatus(personaUid: string) {
    return invoke<ProfileStatus>(COMMANDS.memory.getProfileStatus, { personaUid })
  },
  getEvidence(personaUid: string, traitId: number) {
    return invoke<TraitEvidenceChain[]>(COMMANDS.memory.getEvidence, { personaUid, traitId })
  },
  getFacts(personaUid: string) {
    return invoke<FactList>(COMMANDS.memory.getFacts, { personaUid })
  },
  /** 手动触发全局管线；命令返回 `"ok"` 表示后台已提交 */
  triggerPipeline() {
    return invoke<string>(COMMANDS.memory.triggerPipeline)
  },
}

/** 人格：全字段列表、信息更新、画像重载、预设导入导出、提示词预览 */
export const personaApi = {
  listFull() {
    return invoke<PersonaFull[]>(COMMANDS.persona.listFull)
  },
  /** 更新名称 / 头像 / 描述（缺省字段保持旧值，空字符串表达清空） */
  updateInfo(uid: string, request: PersonaUpdateRequest) {
    return invoke<PersonaFull>(COMMANDS.persona.updateInfo, { uid, request })
  },
  /** 重载画像：触发该人格的 L2→L3 后台管线，返回 `"ok"` */
  refresh(uid: string) {
    return invoke<string>(COMMANDS.persona.refresh, { uid })
  },
  /** 深度处理导入消息：重生成 L1 并级联 L2→L3 */
  regenerateImportPipeline(personaUid: string) {
    return invoke<ImportPipelineReceipt>(COMMANDS.persona.regenerateImportPipeline, {
      personaUid,
    })
  },
  /** 提示词预览：`input` 为空按静态段处理，非空为全量（含召回） */
  previewPrompt(uid: string, input: string | null = null) {
    return invoke<PersonaPromptPreview>(COMMANDS.persona.previewPrompt, { uid, input })
  },
  /** 预设导入预览（dry-run：不写库、不调用模型） */
  previewImport(filePath: string, into: string | null = null, noAi = false) {
    return invoke<PresetImportPreview>(COMMANDS.persona.previewImport, {
      filePath,
      into,
      noAi,
    })
  },
  /** 预设导入执行 */
  importPreset(filePath: string, into: string | null = null, noAi = false) {
    return invoke<PresetImportOutcome>(COMMANDS.persona.importPreset, {
      filePath,
      into,
      noAi,
    })
  },
  /** 导出人格预设；`outputPath` 由 `saveFileDialog` 取得 */
  personaExport(uid: string, withExamples: boolean, outputPath: string) {
    return invoke<PersonaExportReceipt>(COMMANDS.persona.personaExport, {
      uid,
      withExamples,
      outputPath,
    })
  },
}

/** 行为规则：列表、启停、编辑与证据链 */
export const rulesApi = {
  /** 列出该人格的全部规则（含禁用项）；`personaUid` 缺省时后端按默认人格处理 */
  list(personaUid: string | null = null) {
    return invoke<RuleListResponse>(COMMANDS.rules.list, { personaUid })
  },
  /** 启用 / 禁用（禁用会写一次 S1 反馈） */
  setEnabled(ruleId: number, enabled: boolean) {
    return invoke<RuleEnabledReceipt>(COMMANDS.rules.setEnabled, { ruleId, enabled })
  },
  /** 编辑规则文本与禁忌词；保存后转 Manual 强锚点，`null` 表示不提交该字段 */
  edit(ruleId: number, reaction: string | null = null, avoid: string | null = null) {
    return invoke<BehaviorRuleView>(COMMANDS.rules.edit, { ruleId, reaction, avoid })
  },
  /** 规则证据链（规则 → 事件，只读脱敏字段） */
  evidence(ruleId: number) {
    return invoke<RuleEvidenceResponse>(COMMANDS.rules.evidence, { ruleId })
  },
}

/** 导入：格式检测、文件分析、执行、续跑与续跑状态 */
export const importApi = {
  /** 判断是否为 QQ 导出格式（只读检测，不解析文件内容） */
  detectFormat(filePath: string) {
    return invoke<boolean>(COMMANDS.import.detectFormat, { filePath })
  },
  /** 解析报告（不写库）；`gapMinutes` 缺省时后端按 10 处理 */
  analyzeFile(filePath: string, gapMinutes: number | null = null) {
    return invoke<ImportAnalysisReport>(COMMANDS.import.analyzeFile, { filePath, gapMinutes })
  },
  /**
   * 执行导入：L0 同步写入，L1 / 级联在后台进行，进度经 `import-progress` 事件
   *
   * 说明: 空串按 `null` 处理，对齐后端 `Option<String>` 的「沿用解析值」语义
   */
  importQQ(
    filePath: string,
    mode: string | null = null,
    personaName: string | null = null,
    selfPersonaUid: string | null = null,
    otherPersonaName: string | null = null,
    otherPersonaUid: string | null = null,
    gapMinutes: number | null = null,
    side: string | null = null,
  ) {
    return invoke<ImportResult>(COMMANDS.import.importQQ, {
      filePath,
      mode,
      personaName,
      selfPersonaUid,
      otherPersonaName,
      otherPersonaUid,
      gapMinutes,
      side,
    })
  },
  /** 继续处理：立即返回，进度经 `import-progress`；`exportRoot` 为 null 时跳过附件回补 */
  continueImport(exportRoot: string | null = null, resetSkipped: boolean | null = null) {
    return invoke<null>(COMMANDS.import.continueImport, { exportRoot, resetSkipped })
  },
  /** 续跑状态：后处理标志 + 三类待办计数 */
  getStatus() {
    return invoke<ImportStatusView>(COMMANDS.import.getStatus)
  },
}

/** 系统：版本号、文件对话框与发图目录 */
export const systemApi = {
  getVersion() {
    return invoke<string>(COMMANDS.diagnostics.getVersion)
  },
  /** 打开文件对话框；返回所选路径，取消为 `null` */
  openFileDialog(options: {
    initialDir?: string | null
    title?: string | null
    filterName?: string | null
    extensions?: string[] | null
  }) {
    return invoke<string | null>(COMMANDS.system.openFileDialog, {
      initialDir: options.initialDir ?? null,
      title: options.title ?? null,
      filterName: options.filterName ?? null,
      extensions: options.extensions ?? null,
    })
  },
  /** 目录对话框：返回用户所选目录，取消为 `null`（不做目录特征校验） */
  openDirectoryDialog(initialDir: string | null = null, title: string | null = null) {
    return invoke<string | null>(COMMANDS.system.openDirectoryDialog, {
      initialDir,
      title,
    })
  },
  /** 保存对话框：返回用户选定路径，取消为 `null`；参数键为 snake_case */
  saveFileDialog(defaultName: string, filterName: string | null = null) {
    return invoke<string | null>(COMMANDS.system.saveFileDialog, {
      default_name: defaultName,
      filter_name: filterName,
    })
  },
  getImageSourceDir() {
    return invoke<string>(COMMANDS.system.getImageSourceDir)
  },
  setImageSourceDir(path: string | null = null) {
    return invoke<string>(COMMANDS.system.setImageSourceDir, { path })
  },
}

/** MCP 接入：面板信息（配置读写仍走 config 域） */
export const mcpApi = {
  getInfo() {
    return invoke<McpInfo>(COMMANDS.mcp.getInfo)
  },
}

/** 主动对话：名单三态与投递概览 */
export const proactiveApi = {
  listPersonas() {
    return invoke<ProactivePersonaRow[]>(COMMANDS.proactive.listPersonas)
  },
  /** 保存人格主动开关；`mode` 为 auto / on / off */
  setPersona(uid: string, mode: string) {
    return invoke<string>(COMMANDS.proactive.setPersona, { uid, mode })
  },
  getStats() {
    return invoke<ProactiveOverview>(COMMANDS.proactive.getStats)
  },
}

/** 社交通道：状态、启停与绑定 */
export const channelApi = {
  getStatus() {
    return invoke<ChannelPanel>(COMMANDS.channel.getStatus)
  },
  start() {
    return invoke<ChannelPanel>(COMMANDS.channel.start)
  },
  stop() {
    return invoke<ChannelPanel>(COMMANDS.channel.stop)
  },
  listBindings() {
    return invoke<ChannelBinding[]>(COMMANDS.channel.listBindings)
  },
  /** 写入绑定；未提供字段保持现值（`personaUid` 传空串表示清除覆盖） */
  setBinding(
    channel: string,
    externalRef: string,
    personaUid: string | null = null,
    enabled: boolean | null = null,
    allowProactive: boolean | null = null,
  ) {
    return invoke<ChannelBinding>(COMMANDS.channel.setBinding, {
      channel,
      externalRef,
      personaUid,
      enabled,
      allowProactive,
    })
  },
}

/** 数据导出：写入用户选定路径 */
export const exportApi = {
  json(outputPath: string) {
    return invoke<string>(COMMANDS.export.json, { outputPath })
  },
  markdown(outputPath: string) {
    return invoke<string>(COMMANDS.export.markdown, { outputPath })
  },
}

/** 诊断与更新：检查更新、诊断包导出（版本号经 `systemApi.getVersion`） */
export const diagnosticsApi = {
  checkUpdate() {
    return invoke<UpdateStatus>(COMMANDS.diagnostics.checkUpdate)
  },
  exportDiagnostics() {
    return invoke<DiagnosticsExport>(COMMANDS.diagnostics.exportDiagnostics)
  },
}
