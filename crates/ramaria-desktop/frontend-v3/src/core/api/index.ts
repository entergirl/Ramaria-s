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
  ClearCacheResult,
  ConfigWriteReceipt,
  DegradedReason,
  EmbeddingModelView,
  EmbeddingValidation,
  IndexRebuildReceipt,
  IndexStatusView,
  PrivacyStatus,
  SendLocalImageResult,
  SessionDetail,
  SessionSummary,
  SettingItem,
} from './dto'

/** 对话：发送、状态、隐私、撤回、本地发图 */
export const chatApi = {
  send(message: string, personaUid: string | null = null, sessionId: string | null = null) {
    return invoke<string>(COMMANDS.chat.send, { message, personaUid, sessionId })
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

/** 系统：版本号（诊断域在 M7 补齐其余方法） */
export const systemApi = {
  getVersion() {
    return invoke<string>(COMMANDS.diagnostics.getVersion)
  },
}
