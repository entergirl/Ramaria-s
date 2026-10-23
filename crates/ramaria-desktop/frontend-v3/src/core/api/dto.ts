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
 * 形状: snake_case，8 字段
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
