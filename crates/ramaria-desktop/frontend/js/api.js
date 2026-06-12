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
 * - 所有调用基于 TauriBridge.invoke()，不直接访问 window.__TAURI__
 *
 * 用法:
 *   var sessions = await RamariaApi.session.list();
 *   var result = await RamariaApi.chat.send('你好', 'rama-0001', null);
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
     * 获取当前应用状态。
     *
     * 返回:
     * - 状态字符串（snake_case，来自 Rust AppState::as_str()）:
     *   "needs_setup" | "downloading_model" | "indexing" | "ready" | "degraded" | "fatal_error"
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
    // 公开 API
    // =========================================================

    return {
        chat: {
            send: sendMessage,
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
        },
        export: {
            json: exportSessionsJson,
            markdown: exportSessionsMarkdown,
        },
        index: {
            rebuild: rebuildIndex,
        },
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaApi', {
    value: RamariaApi,
    writable: false,
    configurable: false,
});
