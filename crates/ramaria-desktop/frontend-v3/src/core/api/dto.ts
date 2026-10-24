/**
 * src/core/api/dto.ts — 命令返回与事件负载的数据形状
 *
 * 职责:
 * - 手写 DTO，字段名与 Rust 序列化结果逐字对齐，不在传输层做命名转换
 *
 * 设计特点:
 * - 命名沿用后端序列化：多数视图为 snake_case，配置与附件类视图显式 camelCase，
 *   每个类型标注来源命令与字段数，避免「看起来该是 camelCase」的猜测
 * - 时间统一为 Unix 毫秒整数；UUID 统一为字符串
 * - 可选字段用 `?` 表达，对应 Rust 的 `skip_serializing_if`
 * - 只声明当前视图消费的域，其余随 M6 / M7 增量追加
 */

/** 应用状态机（`get_app_state` 返回的字符串） */
export type AppStateValue =
  | 'needs_setup'
  | 'downloading_model'
  | 'ready'
  | 'degraded'
  | 'fatal_error'

/** 索引状态机（独立于应用状态，不参与路由） */
export type IndexStatusValue = 'idle' | 'building' | 'ready' | 'degraded'

/** 降级原因（`get_degraded_reason`；`null` 表示未降级） */
export type DegradedReason =
  | 'embedding_missing'
  | 'llm_unavailable'
  | 'both_unavailable'
  | 'unknown'

/** 消息角色 */
export type MessageRole = 'user' | 'assistant' | 'system'

/**
 * 会话摘要（`list_sessions` / `create_session`）
 *
 * 形状: snake_case，8 字段；`last_message_preview` 为可选追加字段，
 * 后端尚未输出时前端回退消息数（不伪造预览内容）
 */
export interface SessionSummary {
  id: string
  started_at: number
  ended_at: number | null
  message_count: number
  unread: number
  persona_uid: string | null
  channel: string
  external_ref: string | null
  last_message_preview?: string
}

/**
 * 会话内消息（`get_session` 的 `messages` 条目）
 *
 * 形状: snake_case，9 字段；`recalled_at` 非空即为平台已撤回
 */
export interface SessionMessage {
  id: string
  role: string
  content: string
  persona_uid: string | null
  created_at: number
  is_proactive: boolean
  sender_ref: string | null
  sender_name: string | null
  recalled_at: number | null
}

/**
 * 会话详情（`get_session`）
 *
 * 形状: snake_case，8 字段
 */
export interface SessionDetail {
  id: string
  started_at: number
  ended_at: number | null
  persona_uid: string | null
  channel: string
  total_messages: number
  has_more: boolean
  messages: SessionMessage[]
}

/**
 * 索引状态视图（`get_index_status`）
 *
 * 形状: snake_case，8 字段；`model` / `last_error` 缺省不输出
 */
export interface IndexStatusView {
  status: IndexStatusValue
  loaded: number
  total: number
  failed: number
  percent: number
  model?: string
  last_error?: string
  updated_at: number
}

/**
 * 索引构建进度（`index-progress` 事件）
 *
 * 形状: snake_case；每批一发，终态为 `ready` / `degraded`
 */
export interface IndexProgress {
  status: IndexStatusValue
  loaded: number
  total: number
  failed: number
  percent: number
  elapsed_ms: number
  model?: string
}

/** 索引重建回执（`rebuild_index`）：`started=false` 表示已有构建在途 */
export interface IndexRebuildReceipt {
  started: boolean
}

/**
 * 后端配置（`get_backend_config`）
 *
 * 形状: camelCase（Rust 侧显式重命名）；`apiKeyMasked` 为遮罩版本
 */
export interface BackendConfig {
  provider: string
  modelId: string
  baseUrl: string
  supportsStreaming: boolean
  supportsJsonMode: boolean
  contextWindow: number
  maxOutputTokens: number
  apiKeyMasked: string | null
}

/** 设置项（`get_settings` 数组条目）：snake_case */
export interface SettingItem {
  key: string
  value: string
}

/** 图片缓存清理结果（`clear_image_cache`）：camelCase */
export interface ClearCacheResult {
  files: number
  bytes: number
}

/** 本地发图结果（`send_local_image`）：camelCase */
export interface SendLocalImageResult {
  messageId: string
  placeholder: string
  content: string
}

/**
 * 隐私确认状态（`check_privacy`）
 *
 * 形状: `status` 为 PascalCase 变体名，其余字段 snake_case
 */
export type PrivacyStatus =
  | { status: 'NotNeeded' }
  | { status: 'Confirmed'; persistent: boolean; confirmed_at: number }
  | { status: 'NeedsConfirmation'; provider_name: string; base_url: string }
  | { status: 'Unknown' }

/** 聊天增量（`chat-delta` 事件）：snake_case */
export interface ChatDeltaPayload {
  request_id: string
  content: string
}

/** 聊天完成（`chat-done` 事件）：snake_case，`content` 为完整回复 */
export interface ChatDonePayload {
  request_id: string
  backend_id: string | null
  total_chars: number
  content: string
}

/** 聊天错误（`chat-error` 事件）：snake_case */
export interface ChatErrorPayload {
  request_id: string
  error_title: string
  error_detail: string
  retryable: boolean
}

/**
 * 嵌入模型校验结果（`validate_embedding_model`）
 *
 * 形状: snake_case，3 字段；`dimension` / `reason` 缺省不输出
 */
export interface EmbeddingValidation {
  valid: boolean
  dimension?: number
  reason?: string
}

/**
 * 嵌入模型配置视图（`get_embedding_model`）
 *
 * 形状: snake_case；未配置时命令返回 `null`，`model_path` 缺省不输出
 */
export interface EmbeddingModelView {
  model_path?: string
  valid: boolean
  dimension?: number
}

/**
 * 全量配置写回执（`update_full_config`）
 *
 * 形状: camelCase；`failures` 为空表示文件与 DB 双侧都写入成功
 */
export interface ConfigWriteReceipt {
  fileOk: boolean
  dbOk: boolean
  failures: string[]
}

/** 视觉能力单键写回执（`set_vision_support`）：camelCase */
export interface VisionSupportReceipt {
  fileOk: boolean
  dbOk: boolean
  failures: string[]
}

/**
 * 模型下载进度（`model-download-progress` 事件）
 *
 * 形状: camelCase；`status` 四态，`percent` 为 0~100；总量未知时 `total` 与 `percent` 为 0
 */
export interface ModelDownloadProgress {
  status: 'started' | 'progress' | 'completed' | 'failed'
  downloaded: number
  total: number
  percent: number
  elapsedMs: number
  model?: string
}

/** 主动消息（`proactive-message` 事件）：snake_case，`activated` 仅点击重播时存在 */
export interface ProactiveMessagePayload {
  session_id: string
  message_id: string
  content: string
  persona: string
  source: string
  created_at: number
  activated?: boolean
}

/**
 * 人格完整视图（`list_personas_full`）
 *
 * 形状: snake_case，11 字段；`config` 为 JSON 文本，对话视图不消费
 */
export interface PersonaFull {
  uid: string
  name: string
  kind: string
  source: string
  ref_id: string | null
  avatar: string | null
  config: string | null
  description: string | null
  is_active: boolean
  created_at: number
  updated_at: number
}

/**
 * 保存对话回执（`save_current_session`）
 *
 * 形状: 命令返回 JSON 文本，本类型为其解析结果；`no_active_session` 表示无可关闭会话
 */
export interface SaveSessionReceipt {
  status: 'ok' | 'no_active_session'
  l1_generated: boolean
  session_id?: string
  l1_count?: number
  msg_count?: number
}

/**
 * L1 重试回执（`generate_l1`）
 *
 * 形状: 命令返回 JSON 文本；`reason` 仅在未生成时出现
 */
export interface L1RetryReceipt {
  l1_generated: boolean
  summary?: string
  reason?: string
  session_id?: string
}
