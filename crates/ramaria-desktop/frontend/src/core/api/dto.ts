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
 * L1 会话摘要（`get_l1_memories`）
 *
 * 形状: snake_case，12 字段；`occurred_at` 为浏览器基准时间（会话块内最早消息时刻），
 * `context_json` 内为 `chat_partners`（人格 UID 列表）等分组上下文
 */
export interface MemoryL1 {
  id: string
  session_id: string
  summary: string
  keywords: string
  atmosphere: string
  valence: number
  salience: number
  persona_uid: string | null
  created_at: number
  occurred_at: number
  time_period: string | null
  context_json: string | null
}

/**
 * L2 离散事件（`get_l2_events`）
 *
 * 形状: snake_case，14 字段；`start` / `end` 为事件发生区间，`attitude` 缺省为空串
 */
export interface MemoryEvent {
  id: number
  persona_uid: string
  title: string
  summary: string
  keywords: string
  valence: number
  confidence: number
  presentation: string
  share: number
  attitude: string
  salience: number
  created_at: number
  start: number
  end: number
}

/**
 * 人格摘要（`get_personas`）
 *
 * 形状: snake_case，6 字段；记忆页按 `kind` / `source` 派生分类标签
 */
export interface PersonaSummary {
  uid: string
  name: string
  kind: string
  source: string
  is_active: boolean
  created_at: number
}

/**
 * 单条性格标签（`get_personality_profile` 三层内的条目）
 *
 * 形状: snake_case，15 字段；`layer` 为 base / primary / accent
 */
export interface TraitDetail {
  id: number
  label: string
  meaning: string
  confidence: number
  evidence: number
  consistency: number
  layer: string
  not_meaning: string | null
  trigger: string | null
  suppress: string | null
  related: string | null
  seq: number
  source: string
  status: string
  created_at: number
}

/** L3 三层画像（`get_personality_profile`）：按底色 / 主色调 / 点缀分组 */
export interface PersonalityProfile {
  persona_uid: string
  base: TraitDetail[]
  primary: TraitDetail[]
  accent: TraitDetail[]
}

/**
 * 画像数据状态（`get_profile_status`）
 *
 * 形状: snake_case，5 字段；`status` 为 insufficient / preliminary / trusted，
 * `status_text` 为后端提供的中文描述
 */
export interface ProfileStatus {
  persona_uid: string
  n_total_eff: number
  active_trait_count: number
  status: string
  status_text: string
}

/** 证据链中的 L1 溯源（`get_trait_evidence`）：snake_case，6 字段 */
export interface EvidenceL1Source {
  l1_id: string
  summary: string
  evidence_notes: string[]
  atmosphere: string | null
  valence: number
  weight: number
}

/**
 * 证据链中的事件（`get_trait_evidence`）
 *
 * 形状: snake_case；`paraphrase` / `motives` 缺省不输出
 */
export interface EvidenceEvent {
  event_id: number
  title: string
  summary: string
  confidence: number
  valence: number
  salience: number
  attitude: string | null
  paraphrase: string | null
  motives: string | null
  l1_sources: EvidenceL1Source[]
}

/**
 * 单条性格标签的完整证据链（`get_trait_evidence`）
 *
 * 形状: snake_case；命令返回数组，`trait_id` 不存在时为空数组
 */
export interface TraitEvidenceChain {
  trait_id: number
  trait_label: string
  total_evidence: number
  support_count: number
  contradict_count: number
  neutral_count: number
  evidence_events: EvidenceEvent[]
}

/**
 * 知识事实（`get_facts` 的条目）
 *
 * 形状: snake_case；`version_of` 指向被替换的旧事实，`keyword_hint` 缺省不输出
 */
export interface PersonaFact {
  id: number
  field: string
  content: string
  status: string
  tier: string
  confidence: number
  source: string
  keyword_hint: string | null
  version_of: number | null
  created_at: number
}

/**
 * 知识事实查询结果（`get_facts`）
 *
 * 形状: snake_case；`grouped` 以中文字段名为键，`versions` 以事实 id（字符串化）为键
 */
export interface FactList {
  persona_uid: string
  grouped: Record<string, PersonaFact[]>
  versions: Record<string, PersonaFact[]>
}

/**
 * 导入消息深度处理回执（`regenerate_import_pipeline`）
 *
 * 形状: snake_case；`early_terminated` 表示连续失败达上限后提前终止
 */
export interface ImportPipelineReceipt {
  l1_regenerated: number
  l1_failed: number
  total_sessions: number
  early_terminated: boolean
  remaining_skipped: number
  message: string
}

/**
 * 导入成员统计（分析报告与导入结果的 `members` 条目）
 *
 * 形状: snake_case；`uin` 缺失为 null；`message_count` 为该发送者的成功解析条数
 */
export interface ImportMemberStat {
  uid: string
  uin: string | null
  name: string
  message_count: number
}

/**
 * QQ 聊天记录解析报告（`analyze_qq_chat`）
 *
 * 形状: snake_case，17 字段；不写入数据库，仅描述文件内容
 */
export interface ImportAnalysisReport {
  file_path: string
  self_id: string
  self_name: string
  self_uin: string | null
  chat_name: string
  chat_type: string
  members: ImportMemberStat[]
  other_name: string
  other_uid: string
  other_uin: string | null
  time_range: string
  total_raw: number
  total_success: number
  total_degraded: number
  total_skipped: number
  session_count: number
  gap_minutes: number
}

/**
 * QQ 聊天记录导入结果（`import_qq_chat`）
 *
 * 形状: snake_case，16 字段；`session_ids` 为空时不输出；L0 同步完成，L1 / 级联在后台进行
 */
export interface ImportResult {
  success: boolean
  mode: string
  report_summary: string
  sessions_written: number
  messages_written: number
  persona_uid: string | null
  persona_name: string
  other_persona_uid: string | null
  other_persona_name: string
  self_name: string
  chat_name: string
  chat_type: string
  members: ImportMemberStat[]
  time_range: string
  skipped_count: number
  session_ids?: string[]
}

/**
 * 导入处理进度（`import-progress` 事件）
 *
 * 形状: snake_case；`phase` 为 vision / l1 / l2 / l3 / done；
 * 可选字段按阶段输出（`done` 才带 `l1_success` / `l1_failed` / `l2_triggered` / `l3_triggered`）
 */
export interface ImportProgressPayload {
  phase: string
  current: number
  total: number
  message: string
  l1_success?: number
  l1_failed?: number
  l2_triggered?: boolean
  l3_triggered?: boolean
  l1_expected?: number
  l2_expected?: number
  l3_expected?: number
  eta_seconds?: number
  reset_count?: number
  vision_done?: number
  vision_failed?: number
  vision_skipped?: number
}

/**
 * 导入续跑状态（`get_import_status`）
 *
 * 形状: snake_case；`resumable` = 三类待办任一大于 0（由服务层计算）
 */
export interface ImportStatusView {
  post_active: boolean
  pending_attachments: number
  skipped_attachments: number
  missing_l1_sessions: number
  resumable: boolean
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

/**
 * 人格信息更新请求（`update_persona_info` 的 `request`）
 *
 * 形状: snake_case，字段全部可选；缺省字段保持旧值，空字符串表达清空
 */
export interface PersonaUpdateRequest {
  name?: string
  avatar?: string
  description?: string
}

/**
 * 预设导入写入统计（`import_persona_preset` 的 `written`）
 *
 * 形状: snake_case，5 字段；`failed` 为写入失败被跳过的条目数
 */
export interface ImportWriteStats {
  traits: number
  facts: number
  examples: number
  behavior_rules: number
  failed: number
}

/**
 * 预设导入的拆解执行情况
 *
 * 形状: snake_case；`skipped_reason` 缺省不输出（未跳过）
 */
export interface DecomposeExecution {
  requested: boolean
  executed: boolean
  skipped_reason?: string
  fact_count: number
  trait_count: number
}

/**
 * 预设导入预览（`preview_persona_import`）
 *
 * 形状: snake_case；`text` 为服务层渲染的逐条裁决文本，前端不重算；
 * `uid` 为 null 表示待自动生成；`decompose_enabled` / `privacy_confirmed` 供预览提示
 */
export interface PresetImportPreview {
  text: string
  uid: string | null
  mode: string
  name: string
  decompose_enabled: boolean
  privacy_confirmed: boolean
}

/**
 * 预设导入结果（`import_persona_preset`）
 *
 * 形状: snake_case；`written` 为写入统计，`decompose` 为拆解执行情况
 */
export interface PresetImportOutcome {
  text: string
  uid: string
  mode: string
  name: string
  written: ImportWriteStats
  decompose: DecomposeExecution
}

/** 人格导出统计（`persona_export` 的 `stats`）：camelCase，6 字段 */
export interface PersonaExportStats {
  traits: number
  facts: number
  behaviorRules: number
  blocks: number
  promptPatch: boolean
  examplesIncluded: number
}

/**
 * 人格导出回执（`persona_export`）
 *
 * 形状: camelCase；`writtenTo` 为实际写入路径，`hints` 降级说明缺省省略
 */
export interface PersonaExportReceipt {
  uid: string
  name: string
  writtenTo: string
  stats: PersonaExportStats
  hints?: string[]
}

/**
 * 提示词预览（`preview_persona_prompt`）
 *
 * 形状: snake_case；`tier` 为 `static`（静态段）或 `full`（含召回）；
 * `memory_context` 无召回时为 null
 */
export interface PersonaPromptPreview {
  uid: string
  tier: string
  system_prompt: string
  memory_context: string | null
  chars: number
  tokens: number
}

/**
 * 行为规则的情境侧特征（`BehaviorRule.situation` 原样序列化）
 *
 * 形状: snake_case；`centroid` / `response_centroid` 为聚类中心向量，界面不消费，可能为 null
 */
export interface RuleSituation {
  keywords: string[]
  centroid: number[] | null
  response_centroid: number[] | null
  valence_mean: number
  valence_std: number
  sample_count: number
  presentation_dist: Array<{ presentation: string; freq: number }>
  situation_strength_mean: number
  time_span_days: number
  trait_refs: string[]
}

/** 行为规则的结构化参数（`params`），四项均为 0..1 或 -1..1 的浮点 */
export interface RuleParams {
  emotional_intensity: number
  proactiveness: number
  detail_level: number
  formality: number
}

/**
 * 行为规则（`list_rules` / `edit_rule` 的单条）
 *
 * 状态: `reaction` 为 null 表示候选规则，只做参数注入、没有规则文本
 */
export interface BehaviorRuleView {
  id: number
  persona_uid: string
  situation: RuleSituation
  reaction: string | null
  params: RuleParams
  avoid: string[]
  evidence: Array<{ event_id: number; weight: number }>
  confidence: number
  stability: number
  source: 'auto' | 'manual'
  enabled: boolean
  created_at: number
  updated_at: number
}

/** 规则列表响应（`list_rules`）：按人格过滤后的规则与总数 */
export interface RuleListResponse {
  persona_uid: string
  total: number
  rules: BehaviorRuleView[]
}

/** 规则证据项（`rule_evidence`）：只含脱敏结构化字段，不含对话原文 */
export interface RuleEvidenceItem {
  event_id: number
  weight: number
  title: string
  summary: string
  paraphrase: string | null
  keywords: string | null
}

/** 规则证据链响应（`rule_evidence`） */
export interface RuleEvidenceResponse {
  rule_id: number
  evidence: RuleEvidenceItem[]
}

/** 规则启停回执（`set_rule_enabled`） */
export interface RuleEnabledReceipt {
  id: number
  enabled: boolean
}

/**
 * MCP 接入信息（`get_mcp_info`）
 *
 * 形状: camelCase；`lastActivityMs` 为 mcp 通道最近消息时间，无活动为 null
 */
export interface McpInfo {
  enabled: boolean
  dbPath: string
  configPath: string
  command: string
  commandIsBundled: boolean
  activeSessions: number
  lastActivityMs: number | null
}

/**
 * 单通道运行状态（`get_channel_status` 的 `channels[]` 与 `channel-status` 事件共用）
 *
 * 形状: camelCase；`state` 为 disconnected / connecting / online / error
 */
export interface ChannelStatus {
  channel: string
  state: string
  lastError: string | null
  updatedAt: number
}

/**
 * 通道面板视图（`get_channel_status` / `start_channel` / `stop_channel`）
 *
 * 形状: camelCase；凭据只回显「是否已配置」，不含明文
 */
export interface ChannelPanel {
  enabled: boolean
  qqEnabled: boolean
  telegramEnabled: boolean
  qqWsUrl: string
  defaultPersonaUid: string
  proactiveDeliveryEnabled: boolean
  qqTokenConfigured: boolean
  telegramTokenConfigured: boolean
  running: boolean
  channels: ChannelStatus[]
}

/**
 * 通道绑定（`list_channel_bindings` / `set_channel_binding`）
 *
 * 形状: camelCase；`personaUid` 为绑定级覆盖（null = 未覆盖）
 */
export interface ChannelBinding {
  channel: string
  externalRef: string
  personaUid: string | null
  effectivePersonaUid: string
  enabled: boolean
  allowProactive: boolean
  createdAt: number
  updatedAt: number
}

/**
 * 主动消息名单条目（`list_proactive_personas`）
 *
 * 形状: snake_case；`mode` 三态 auto / on / off，`effective` 为服务层结论
 */
export interface ProactivePersonaRow {
  uid: string
  name: string
  kind: string
  has_local_dialogue: boolean
  mode: string
  effective: boolean
}

/** 单日投递计数（投递概览）：本地日期 `YYYY-MM-DD` */
export interface ProactiveDailyRow {
  date: string
  count: number
}

/** 投递与回应合计（窗口 0 = 不限；比率为空表示无投递） */
export interface ProactiveTotals {
  personas: number
  deliveries: number
  responded: number
  response_rate: number | null
  median_response_ms: number | null
}

/**
 * 投递概览（`get_proactive_stats`）
 *
 * 形状: snake_case；`daily` 为全人格按日期升序聚合；调度内部状态不在其中
 */
export interface ProactiveOverview {
  generated_at: number
  daily_limit: number
  daily_total_limit: number
  daily_count: number
  daily_date: string
  daily: ProactiveDailyRow[]
  last_sent_at: number | null
  cooldown_hours: number
  silence_streak_max: number
  judge_yes_count: number
  judge_no_count: number
  last_judge_at: number | null
  totals: ProactiveTotals
}

/**
 * 版本检查结果（`check_update`）
 *
 * 形状: camelCase；检查失败时 `error` 有值，其余字段可能为 null
 */
export interface UpdateStatus {
  currentVersion: string
  latestVersion: string | null
  updateAvailable: boolean
  releaseUrl: string | null
  releaseNotesPreview: string | null
  error: string | null
}

/**
 * 诊断导出回执（`export_diagnostics`）
 *
 * 形状: camelCase；路径由原生保存对话框选出，前端不传入
 */
export interface DiagnosticsExport {
  outputPath: string
  fileSizeBytes: number
  fileSizeDisplay: string
  collectionStatus: Record<string, string>
  warnings: string[]
}

/**
 * 单条关键词词条（`list_keywords` 的 `keywords` 条目）
 *
 * 形状: snake_case，6 字段；`status` 为 canonical / alias / pending，
 * 规范词自身的 `canonical_id` 与 `canonical_keyword` 为 `null`
 */
export interface KeywordEntry {
  keyword: string
  use_count: number
  status: string
  canonical_id: number | null
  canonical_keyword: string | null
  created_at: number
}

/**
 * 关键词池（`list_keywords`）
 *
 * 形状: snake_case；`keywords` 为全量词条（按使用次数降序），三态计数另行给出
 */
export interface KeywordPool {
  total: number
  canonical_count: number
  alias_count: number
  pending_count: number
  keywords: KeywordEntry[]
}

/** 待确认别名（`list_pending_aliases` 的 `aliases` 条目）：snake_case */
export interface PendingAlias {
  alias_id: number
  alias_keyword: string
  canonical_keyword: string
  created_at: number
}

/** 待确认别名列表（`list_pending_aliases`）：snake_case */
export interface PendingAliasList {
  total: number
  aliases: PendingAlias[]
}

/**
 * 别名裁决结果（`resolve_alias`）
 *
 * 形状: snake_case；`status` 为处理后的词条状态（alias / canonical），
 * `already_applied` 为真表示此前已合并、本次未写库
 */
export interface AliasResolveResult {
  alias: string
  canonical_keyword: string | null
  status: string
  already_applied: boolean
}

/**
 * 说话风格统计（`get_style_stats`；无记录时为 `null`）
 *
 * 形状: snake_case，10 字段；`stats_json` 为五维统计参数原始文本，
 * 状态与规则来源的中文标签由后端随视图给出
 */
export interface StyleStats {
  persona_uid: string
  sample_count: number
  baseline_version: number
  status: string
  status_label: string
  rule_source: string
  rule_source_label: string
  rule_text: string | null
  stats_json: string
  updated_at: number
}

/** 评估产物文件（`list_eval_files` 的 `files` 条目）：snake_case */
export interface EvalFile {
  name: string
  path: string
  size: number
  modified_at: number | null
}

/** 评估产物文件列表（`list_eval_files`）：snake_case，按修改时间倒序 */
export interface EvalFileList {
  dir: string
  total: number
  files: EvalFile[]
}
