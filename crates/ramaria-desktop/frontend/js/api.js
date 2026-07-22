/**
 * js/api.js — Ramaria TauriBridge 上层通信封装
 *
 * 职责:
 * - 将全部 21 个 Tauri Command 封装为语义化的 API 方法
 * - 统一错误包装：Rust 返回的字符串错误转换为 Error 对象，含中文友好消息
 * - 不操作 DOM，不管理状态（状态由 Store 管理）
 * - 所有方法返回 Promise，调用方可用 async/await 或 .then/.catch
 *
 * 设计特点:
 * - 按功能域分组：chat / session / memory / config / setup / export / index
 * - 参数校验在前，减少无效的 IPC 调用
 * - 每个方法内部 try-catch，将 TauriBridge 底层异常统一包装
 * - 日志使用 [Api] 前缀，便于调试追踪
 * - 所有调用基于 TauriBridge.invoke，不直接访问 window.__TAURI__
 *
 * 用法:
 * var sessions = await RamariaApi.session.list;
 * var result = await RamariaApi.chat.send('你好', 'rama-0001', null);
 *
 * 依赖:
 * - TauriBridge（js/tauri-bridge.js，必须先于本文件加载）
 */

var RamariaApi = (function () {
    'use strict';

 // =========================================================
 // 辅助函数
 // =========================================================

 /**
 * 包装 invoke 调用，统一错误处理和日志。
 *
 * 参数:
 * - `command`: Tauri 命令名
 * - `args`: 命令参数对象
 * - `context`: 调用上下文描述（用于错误日志）
 *
 * 返回:
 * - Promise，成功返回命令结果，失败抛出 Error
 */
    async function _invoke(command, args, context) {
        try {
            var result = await TauriBridge.invoke(command, args || {});
            console.log('[Api] ✓ ' + context);
            return result;
        } catch (err) {
            var message = err.message || String(err);
            console.error('[Api] ✗ ' + context + ': ' + message);
            throw new Error(context + '失败: ' + message);
        }
    }

 /**
 * 参数非空校验。
 * 如果值为空字符串或 undefined/null，抛出错误。
 */
    function _require(val, name) {
        if (val === undefined || val === null || (typeof val === 'string' && val.trim() === '')) {
            throw new Error(name + '不能为空');
        }
    }

 // =========================================================
 // 1. 聊天 (chat)
 // =========================================================

 /**
 * 发送消息并启动流式对话。
 *
 * 参数:
 * - `message`: 用户输入文本（不可为空）
 * - `personaUid`: 可选，指定对话人格 UID
 * - `sessionId`: 可选，复用已有会话 ID
 *
 * 返回:
 * - { request_id: string }，用于关联后续 chat-delta/chat-done/chat-error 事件
 */
    async function sendMessage(message, personaUid, sessionId) {
        _require(message, '消息内容');

        var args = { message: message };
        if (personaUid) args.personaUid = personaUid;
        if (sessionId) args.sessionId = sessionId;

        var requestId = await _invoke('send_message', args, '发送消息');
        return { request_id: requestId };
    }

 /**
 * 手动保存当前活跃会话（关闭 → 生成 L1 摘要）。
 *
 * 参数:
 * - `personaUid`: 当前对话人格 UID，用于 L1 摘要归属。
 *
 * 对齐 save_current_session Tauri Command。
 *
 * 返回:
 * - "ok" 表示保存成功
 */
    async function saveCurrentSession(personaUid) {
        var args = {};
        if (personaUid) args.personaUid = personaUid;
        return await _invoke('save_current_session', args, '保存当前会话');
    }

 /**
 * 为指定 session 重新生成 L1 摘要（手动重试）。
 *
 * 参数:
 * - `sessionId`: 目标 session UUID。
 * - `personaUid`: 可选人格标识。
 *
 * 返回:
 * - { l1_generated: bool, summary?: string, session_id: string }
 */
    async function generateL1(sessionId, personaUid) {
        _require(sessionId, '会话 ID');
        var args = { sessionId: sessionId };
        if (personaUid) args.personaUid = personaUid;
        return await _invoke('generate_l1', args, '重新生成 L1 摘要');
    }

 /**
 * 获取当前应用状态。
 *
 * 返回:
 * - 状态字符串（snake_case，来自 Rust AppState::as_str）:
 * "needs_setup" | "downloading_model" | "indexing" | "ready" | "degraded" | "fatal_error"
 */
    async function getAppState() {
        return await _invoke('get_app_state', {}, '获取应用状态');
    }

 /**
 * 检查隐私确认状态。
 *
 * 返回:
 * - { status: "NotNeeded" | "Confirmed" | "NeedsConfirmation", persistent?, confirmed_at?, provider_name?, base_url? }
 */
    async function checkPrivacy() {
        return await _invoke('check_privacy', {}, '检查隐私状态');
    }

 /**
 * 记录隐私确认。
 *
 * 参数:
 * - `persistent`: 是否持久化（跨重启记住）
 */
    async function confirmPrivacy(persistent) {
        return await _invoke('confirm_privacy', { persistent: !!persistent }, '记录隐私确认');
    }

 // =========================================================
 // 2. 会话管理 (session)
 // =========================================================

 /**
 * 列出所有会话（按开始时间倒序）。
 *
 * 返回:
 * - [{ id, started_at, ended_at, message_count }]
 */
    async function listSessions() {
        return await _invoke('list_sessions', {}, '查询会话列表');
    }

 /**
 * 获取会话详情（含消息列表）。
 *
 * 参数:
 * - `sessionId`: 会话 UUID 字符串
 *
 * 返回:
 * - { id, started_at, ended_at, messages: [{ id, role, content, persona_uid, created_at }] }
 */
    async function getSession(sessionId) {
        _require(sessionId, '会话 ID');
        return await _invoke('get_session', { sessionId: sessionId }, '查询会话详情');
    }

 /**
 * 创建新会话。
 *
 * 返回:
 * - { id, started_at, ended_at, message_count }
 */
    async function createSession() {
        return await _invoke('create_session', {}, '创建会话');
    }

 /**
 * 删除会话及其关联消息。
 *
 * 参数:
 * - `sessionId`: 会话 UUID 字符串
 */
    async function deleteSession(sessionId) {
        _require(sessionId, '会话 ID');
        return await _invoke('delete_session', { sessionId: sessionId }, '删除会话');
    }

 // =========================================================
 // 3. 记忆查看 (memory)
 // =========================================================

 /**
 * 列出所有已注册人格。
 *
 * 返回:
 * - [{ uid, name, kind, is_active, created_at }]
 */
    async function getPersonas() {
        return await _invoke('get_personas', {}, '查询人格列表');
    }

 /**
 * 查询 L1 会话摘要记忆。
 *
 * 参数:
 * - `personaUid`: 可选，按人格过滤
 * - `limit`: 可选，返回条数上限（默认 50，最大 200）
 *
 * 返回:
 * - [{ id, session_id, summary, keywords, atmosphere, valence, salience, persona_uid, created_at }]
 */
    async function getL1Memories(personaUid, limit) {
        var args = {};
        if (personaUid) args.personaUid = personaUid;
        if (limit !== undefined && limit !== null) args.limit = limit;
        return await _invoke('get_l1_memories', args, '查询 L1 记忆');
    }

 /**
 * 查询 L2 离散事件记忆。
 *
 * 参数:
 * - `personaUid`: 可选，按人格过滤
 * - `limit`: 可选，返回条数上限（默认 50，最大 200）
 *
 * 返回:
 * - [{ id, persona_uid, title, summary, keywords, valence, confidence, presentation, share, attitude, salience, created_at }]
 */
    async function getL2Events(personaUid, limit) {
        var args = {};
        if (personaUid) args.personaUid = personaUid;
        if (limit !== undefined && limit !== null) args.limit = limit;
        return await _invoke('get_l2_events', args, '查询 L2 事件');
    }

 /**
 * 查询 L3 性格画像标签。
 *
 * 参数:
 * - `personaUid`: 可选，按人格过滤
 *
 * 返回:
 * - [{ id, persona_uid, layer, label, meaning, confidence, evidence, consistency, status, created_at }]
 */
    async function getL3Traits(personaUid) {
        var args = {};
        if (personaUid) args.personaUid = personaUid;
        return await _invoke('get_l3_traits', args, '查询 L3 性格标签');
    }

// =========================================================
// L3 性格画像查询
// =========================================================

/**
 * 查询指定人格的完整三层性格画像（base/primary/accent）。
 *
 * 参数:
 * - `personaUid`: 目标人格 UID（如 "user-0001"）
 *
 * 返回:
 * - { persona_uid, base: [...], primary: [...], accent: [...] }
 * - 每层数组元素: { id, label, meaning, confidence, evidence, consistency,
 *     layer, not_meaning, trigger, suppress, related, seq, source, status, created_at }
 */
    async function getPersonalityProfile(personaUid) {
        _require(personaUid, 'personaUid');
        return await _invoke('get_personality_profile', { personaUid: personaUid }, '查询 L3 性格画像');
    }

/**
 * 查询指定性格标签的完整证据溯源链。
 *
 * 参数:
 * - `personaUid`: 目标人格 UID。
 * - `traitId`: 目标性格标签 ID。
 *
 * 返回:
 * - [{ trait_id, trait_label, total_evidence, support_count, contradict_count, neutral_count,
 *     evidence_events: [...] }]
 * - evidence_events 每项: { event_id, title, summary, confidence, valence,
 *     salience, attitude, paraphrase, motives, l1_sources: [...] }
 * - l1_sources 每项: { l1_id, summary, evidence_notes: [...], atmosphere, valence, weight }
 */
    async function getTraitEvidence(personaUid, traitId) {
        _require(personaUid, 'personaUid');
        _require(traitId, 'traitId');
        return await _invoke('get_trait_evidence', {
            personaUid: personaUid,
            traitId: traitId,
        }, '查询 trait 证据链');
    }

/**
 * 查询指定人格的数据画像状态。
 *
 * 参数:
 * - `personaUid`: 目标人格 UID。
 *
 * 返回:
 * - { persona_uid, n_total_eff, active_trait_count, status, status_text }
 * - status: "insufficient" (n<5) / "preliminary" (5-20) / "trusted" (≥20)
 */
    async function getProfileStatus(personaUid) {
        _require(personaUid, 'personaUid');
        return await _invoke('get_profile_status', { personaUid: personaUid }, '查询画像数据状态');
    }

 /**
 * 手动触发记忆管线（L2 事件提取 → L3 性格推断）。
 *
 * 说明:
 * - 遍历所有 persona，对满足条件的触发 L2/L3 处理。
 * - 适用于快速导入后手动启动深度处理。
 *
 * 返回:
 * - "ok" 表示管线已触发（后台异步执行）
 */
    async function triggerMemoryPipeline() {
        return await _invoke('trigger_memory_pipeline', {}, '触发记忆管线');
    }

 /**
 * 重新生成导入 persona 的 L1 摘要并级联 L2/L3。
 *
 * 说明:
 * - 对指定 persona 的所有导入 session 重新生成 L1 摘要（persona_uid=NULL）。
 * - L1 生成完成后自动触发 L2→L3 级联（后台异步）。
 * - 适用于导入时 LLM 不可用导致 L1 失败的场景。
 *
 * 参数:
 * - `personaUid`: 目标导入 persona 的 UID（如 "char-123456789"）
 *
 * 返回:
 * - `{ l1_regenerated: N, l1_failed: N, total_sessions: N, message: "..." }`
 */
    async function regenerateImportPipeline(personaUid) {
        _require(personaUid, 'persona UID');
        return await _invoke('regenerate_import_pipeline', { personaUid: personaUid }, '重新生成导入管线');
    }

 // =========================================================
 // 4. 配置管理 (config)
 // =========================================================

 /**
 * 获取后端配置（不含 API key）。
 *
 * 返回:
 * - { provider, model_id, base_url, supports_streaming, supports_json_mode, context_window, max_output_tokens }
 */
    async function getBackendConfig() {
        return await _invoke('get_backend_config', {}, '查询后端配置');
    }

 /**
 * 更新后端配置。
 *
 * 参数:
 * - `provider`: "LmStudio" | "DeepSeek" | "OpenAI"
 * - `modelId`: 模型标识
 * - `baseUrl`: API 基础地址
 * - `apiKey`: 可选，线上 provider 的 API key
 */
    async function updateBackendConfig(provider, modelId, baseUrl, apiKey) {
        _require(provider, 'Provider');
        _require(baseUrl, 'Base URL');

        var args = {
            provider: provider,
            modelId: modelId || '',
            baseUrl: baseUrl,
        };
        if (apiKey) args.apiKey = apiKey;

        return await _invoke('update_backend_config', args, '更新后端配置');
    }

 /**
 * 获取所有全局设置。
 *
 * 返回:
 * - [{ key, value }]
 */
    async function getSettings() {
        return await _invoke('get_settings', {}, '查询全局设置');
    }

 /**
 * 更新或创建单个全局设置项。
 *
 * 参数:
 * - `key`: 设置键名
 * - `value`: 设置值
 */
    async function updateSetting(key, value) {
        _require(key, '设置键名');
        return await _invoke('update_setting', { key: key, value: value || '' }, '更新设置');
    }

 // =========================================================
 // 5. 首次配置 (setup)
 // =========================================================

 /**
 * 执行首次配置和 LLM 连接验证。
 *
 * 参数:
 * - `provider`: "LmStudio" | "DeepSeek" | "OpenAI"
 * - `modelId`: 模型标识（LM Studio 可为空）
 * - `baseUrl`: API 基础地址
 * - `apiKey`: 可选，线上 provider 的 API key
 *
 * 返回:
 * - 状态字符串，如 "setup_complete:Ready"
 */
    async function runSetup(provider, modelId, baseUrl, apiKey) {
        _require(provider, 'Provider');
        _require(baseUrl, 'Base URL');

        var args = {
            provider: provider,
            modelId: modelId || '',
            baseUrl: baseUrl,
        };
        if (apiKey) args.apiKey = apiKey;

        return await _invoke('run_setup', args, '执行首次配置');
    }

 /**
 * 查询设置状态详情。
 *
 * 返回:
 * - { backend_configured, model_selected, needs_indexing, is_complete, missing_items, current_state }
 */
    async function getSetupStatus() {
        return await _invoke('get_setup_status', {}, '查询设置状态');
    }

 /**
 * 刷新应用状态机。
 *
 * 返回:
 * - 新的应用状态字符串
 */
    async function refreshSetupState() {
        return await _invoke('refresh_setup_state', {}, '刷新应用状态');
    }

 /**
 * 校验嵌入模型路径。
 *
 * 参数:
 * - `path`: 模型文件夹绝对路径
 *
 * 返回:
 * - { valid: bool, dimension?: number, reason?: string }
 */
    async function validateEmbeddingModel(path) {
        _require(path, '模型路径');
        return await _invoke('validate_embedding_model', { path: path }, '校验嵌入模型');
    }

 /**
 * 保存嵌入模型配置。
 *
 * 参数:
 * - `path`: 模型文件夹绝对路径（空字符串表示移除）
 */
    async function saveEmbeddingModel(path) {
        return await _invoke('save_embedding_model', { path: path || '' }, '保存嵌入模型配置');
    }

 /**
 * 获取嵌入模型配置。
 *
 * 返回:
 * - { modelPath?: string, valid?: bool, dimension?: number } | null
 */
    async function getEmbeddingModel() {
        return await _invoke('get_embedding_model', {}, '查询嵌入模型配置');
    }

 /**
 * 获取当前 Degraded 状态的详细原因。
 *
 * 返回:
 * - "embedding_missing" | "llm_unavailable" | "unknown" | null
 */
    async function getDegradedReason() {
        return await _invoke('get_degraded_reason', {}, '查询降级原因');
    }

 /**
 * 测试 LLM 连接是否可达。
 *
 * 说明:
 * - 与 refreshSetupState 不同：此方法真正测试 LLM 端点可达性。
 * - 调用前需先通过 updateBackendConfig 或 update_backend_config 保存配置。
 *
 * 返回:
 * - "ok": LLM 连接正常
 * - 否则抛出 Error
 */
    async function testLlmConnection() {
        return await _invoke('test_llm_connection', {}, '测试 LLM 连接');
    }

 // =========================================================
 // 6. 数据导出 (export)
 // =========================================================

 /**
 * 导出全部会话为 JSON 文件。
 *
 * 参数:
 * - `outputPath`: 输出文件路径
 *
 * 返回:
 * - 导出文件的绝对路径
 */
    async function exportSessionsJson(outputPath) {
        _require(outputPath, '导出路径');
        return await _invoke('export_sessions_json', { output_path: outputPath }, '导出 JSON');
    }

 /**
 * 导出全部会话为 Markdown 文件。
 *
 * 参数:
 * - `outputPath`: 输出文件路径
 *
 * 返回:
 * - 导出文件的绝对路径
 */
    async function exportSessionsMarkdown(outputPath) {
        _require(outputPath, '导出路径');
        return await _invoke('export_sessions_markdown', { output_path: outputPath }, '导出 Markdown');
    }

 // =========================================================
 // 7. 索引管理 (index)
 // =========================================================

 /**
 * 触发检索索引全量重建。
 *
 * 返回:
 * - 重建的文档数量
 */
    async function rebuildIndex() {
        return await _invoke('rebuild_index', {}, '重建索引');
    }

 // =========================================================
 // 8. 人格管理 (persona) — 新增
 // =========================================================

 /**
 * 列出所有已注册人格的完整信息（含全字段）。
 *
 * 返回:
 * - [{ uid, name, kind, source, ref_id, avatar, config, description, is_active, created_at, updated_at }]
 */
    async function listPersonasFull() {
        return await _invoke('list_personas_full', {}, '查询人格完整列表');
    }

 /**
 * 更新指定人格的基本信息。
 *
 * 参数:
 * - `uid`: 人格业务标识
 * - `request`: { name?, avatar?, description? } — 所有字段可选，传入 null/undefined 表示不更新
 *
 * 返回:
 * - 更新后的 PersonaFullView
 */
    async function updatePersonaInfo(uid, request) {
        _require(uid, '人格 UID');
        return await _invoke('update_persona_info', { uid: uid, request: request || {} }, '更新人格信息');
    }

 /**
 * 刷新指定人格的记忆管线（L2 事件提取 → L3 性格推断）。
 *
 * 参数:
 * - `uid`: 目标人格业务标识
 *
 * 返回:
 * - "ok": 管线已触发（后台异步执行）
 */
    async function refreshPersona(uid) {
        _require(uid, '人格 UID');
        return await _invoke('refresh_persona', { uid: uid }, '刷新人格管线');
    }

 // =========================================================
 // 9. 数据导入 (import) —
 // =========================================================

 /**
 * 检测文件是否为 QQ 聊天记录格式。
 *
 * 参数:
 * - `filePath`: 文件绝对路径
 *
 * 返回:
 * - true 表示文件可以解析，false 表示格式不匹配
 */
    async function detectQQFormat(filePath) {
        _require(filePath, '文件路径');
        return await _invoke('detect_qq_format', { filePath: filePath }, '检测 QQ 格式');
    }

 /**
 * 解析 QQ 聊天记录文件并返回分析报告（不导入数据）。
 *
 * 参数:
 * - `filePath`: 文件绝对路径
 * - `gapMinutes`: session 切割间隔（分钟），默认 10
 *
 * 返回:
 * - { selfName, selfId, chatName, timeRange, totalSuccess, totalDegraded, totalSkipped, sessionCount, ... }
 */
    async function analyzeQQFile(filePath, gapMinutes) {
        _require(filePath, '文件路径');
        var args = { filePath: filePath };
        if (gapMinutes !== undefined && gapMinutes !== null) {
            args.gapMinutes = gapMinutes;
        }
        return await _invoke('analyze_qq_chat', args, '分析 QQ 文件');
    }

 /**
 * 执行 QQ 聊天记录导入。
 *
 * 参数:
 * - `filePath`: 文件绝对路径
 * - `mode`: 导入模式，"fast"（仅 L0）或 "deep"（全管线）
 * - `personaName`: 可选，导出者 persona 显示名称
 * - `selfPersonaUid`: 可选，导出者 persona UID（留空自动生成）
 * - `otherPersonaName`: 可选，对方 persona 显示名称
 * - `otherPersonaUid`: 可选，对方 persona UID（留空自动生成）
 * - `gapMinutes`: 可选，session 切割间隔（分钟），默认 10
 *
 * 返回:
 * - ImportResult: { success, mode, report_summary, sessions_written, messages_written,
 * persona_uid, persona_name, other_persona_uid, other_persona_name, ... }
 */
    async function importQQChat(filePath, mode, personaName, selfPersonaUid, otherPersonaName, otherPersonaUid, gapMinutes) {
        _require(filePath, '文件路径');
        var args = {
            filePath: filePath,
            mode: mode || 'fast',
        };
        if (personaName) args.personaName = personaName;
        if (selfPersonaUid) args.selfPersonaUid = selfPersonaUid;
        if (otherPersonaName) args.otherPersonaName = otherPersonaName;
        if (otherPersonaUid) args.otherPersonaUid = otherPersonaUid;
        if (gapMinutes !== undefined && gapMinutes !== null) {
            args.gapMinutes = gapMinutes;
        }
        console.log('[Api] 调用 importQQChat，参数:', JSON.stringify(args, null, 2));
        return await _invoke('import_qq_chat', args, '导入 QQ 聊天记录');
    }

 // =========================================================
 // 10. 诊断与更新 (diagnostics) — 新增
 // =========================================================

 /**
 * 检查是否有新版本可用。
 *
 * 说明:
 * - 调用 GitHub Release API 查询最新版本标签。
 * - 与当前运行版本做 semver 比较。
 * - 网络异常时返回 currentVersion + error 字段，不抛出异常。
 * - ⚠️ 此命令会消耗 GitHub API 配额（60次/小时），仅应在用户手动点击时调用。
 *
 * 返回:
 * - { currentVersion, latestVersion?, updateAvailable, releaseUrl?, releaseNotesPreview?, error? }
 */
    async function checkUpdate() {
        return await _invoke('check_update', {}, '检查更新');
    }

 /**
 * 获取当前应用版本号（纯本地，无网络请求）。
 *
 * 说明:
 * - 直接返回编译时嵌入的版本号。
 * - 不消耗 GitHub API 配额。
 *
 * 返回:
 * - 版本号字符串，如 "1.2.0"
 */
    async function getVersion() {
        return await _invoke('get_version', {}, '获取版本号');
    }

 /**
 * 导出诊断信息为 .zip 文件。
 *
 * 说明:
 * - 弹出原生保存对话框，默认文件名为 ramaria-diagnostics-{日期}.zip。
 * - 收集：最近 1000 行日志 + 配置文件（API Key 已脱敏）+ 系统信息 + schema 版本。
 * - 用户选择保存路径后开始打包，完成后返回文件信息。
 *
 * 返回:
 * - { outputPath, fileSizeBytes, fileSizeDisplay }
 */
    async function exportDiagnostics() {
        return await _invoke('export_diagnostics', {}, '导出诊断信息');
    }

 // =========================================================
 // 公开 API
 // =========================================================

    return {
        chat: {
            send: sendMessage,
            save: saveCurrentSession,
            generateL1: generateL1,
            getAppState: getAppState,
            checkPrivacy: checkPrivacy,
            confirmPrivacy: confirmPrivacy,
        },
        session: {
            list: listSessions,
            get: getSession,
            create: createSession,
            delete: deleteSession,
        },
        memory: {
            getPersonas: getPersonas,
            getL1: getL1Memories,
            getL2: getL2Events,
            getL3: getL3Traits,
            getProfile: getPersonalityProfile,
            getEvidence: getTraitEvidence,
            getProfileStatus: getProfileStatus,
            triggerPipeline: triggerMemoryPipeline,
            regenerateImportPipeline: regenerateImportPipeline,
        },
        config: {
            getBackend: getBackendConfig,
            updateBackend: updateBackendConfig,
            getSettings: getSettings,
            updateSetting: updateSetting,
        },
        setup: {
            run: runSetup,
            getStatus: getSetupStatus,
            refresh: refreshSetupState,
            validateEmbeddingModel: validateEmbeddingModel,
            saveEmbeddingModel: saveEmbeddingModel,
            getEmbeddingModel: getEmbeddingModel,
            getDegradedReason: getDegradedReason,
            testLlmConnection: testLlmConnection,
        },
        export: {
            json: exportSessionsJson,
            markdown: exportSessionsMarkdown,
        },
        index: {
            rebuild: rebuildIndex,
        },
        import: {
            detectFormat: detectQQFormat,
            analyzeFile: analyzeQQFile,
            importQQ: importQQChat,
        },
        persona: {
            listFull: listPersonasFull,
            updateInfo: updatePersonaInfo,
            refresh: refreshPersona,
        },
        diagnostics: {
            checkUpdate: checkUpdate,
            getVersion: getVersion,
            exportDiagnostics: exportDiagnostics,
        },
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaApi', {
    value: RamariaApi,
    writable: false,
    configurable: false,
});
