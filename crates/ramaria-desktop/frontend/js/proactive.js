/**
 * js/proactive.js — Ramaria 主动消息处理
 *
 * 职责:
 * - 监听后端 `proactive-message` 事件，处理应用内展示与通知点击定位
 * - 常规消息：刷新会话列表 + 当前会话匹配时把消息幂等追加到 Store
 * - 已读标记：用户正在查看的会话收到消息时标记已读，避免自己眼前的会话出现未读
 * - 通知点击重播（`activated === true`）：切换到目标会话并定位到该消息所在会话
 *
 * 设计特点:
 * - 只操作数据层（Store / Api / Router / ChatView），不直接操作 DOM；
 *   消息气泡重渲染由 chat.js 对 Store.messages 的订阅驱动
 * - 幂等：按 `message_id` 去重，同一事件重复到达不产生重复气泡
 * - 防御性边界：TauriBridge 未定义、负载非对象、字段缺失、Promise 失败
 *   均降级处理，不向上抛出异常
 * - 非 Tauri 环境（浏览器预览）自动跳过监听
 *
 * 用法:
 *   RamariaProactive.init();              // 应用启动时调用一次（app.js）
 *   RamariaProactive.handlePayload(payload); // 处理一条事件负载（可独立调用）
 *
 * 依赖:
 * - TauriBridge（js/tauri-bridge.js）
 * - RamariaStore（js/store.js）
 * - RamariaApi（js/api.js）
 * - RamariaRouter（js/router.js）
 * - RamariaChatView（js/views/chat.js，仅通知点击定位时按需访问）
 */

var RamariaProactive = (function () {
    'use strict';

    // =========================================================
    // 常量
    // =========================================================

    /** 后端主动消息事件名 */
    var EVENT_PROACTIVE_MESSAGE = 'proactive-message';

    // =========================================================
    // 内部状态
    // =========================================================

    /** 取消监听函数（事件监听注册成功后留存） */
    var _unlisten = null;

    /** 是否已发起注册（Promise 未完成期间防止重复注册） */
    var _initStarted = false;

    // =========================================================
    // 事件监听
    // =========================================================

    /**
     * 初始化主动消息事件处理。
     *
     * 说明:
     * - 非 Tauri 环境跳过（浏览器预览模式无事件源）；
     * - 已注册 / 注册中时跳过，避免重复监听。
     */
    function init() {
        if (typeof TauriBridge === 'undefined' || !TauriBridge.isTauri || !TauriBridge.isTauri()) {
            console.warn('[Proactive] 非 Tauri 环境，跳过主动消息监听');
            return;
        }
        if (_initStarted || _unlisten) return;

        _initStarted = true;
        TauriBridge.listen(EVENT_PROACTIVE_MESSAGE, function (event) {
            handlePayload(event && event.payload);
        }).then(function (unlisten) {
            _unlisten = unlisten;
        }).catch(function (err) {
            console.warn('[Proactive] 主动消息事件监听注册失败:', (err && err.message) || err);
        });
    }

    // =========================================================
    // 事件处理
    // =========================================================

    /**
     * 处理一条主动消息事件负载。
     *
     * 参数:
     * - `payload`: 事件负载（snake_case）:
     *   { session_id, message_id, content, persona, source, created_at, activated? }
     *
     * 说明:
     * - `activated === true`：通知点击重播——先按当前会话幂等追加，再切换定位；
     * - 常规消息：刷新会话列表（供抽屉/状态栏更新）+ 幂等追加。
     */
    function handlePayload(payload) {
        if (!payload || typeof payload !== 'object') {
            console.warn('[Proactive] 无效的主动消息负载，已忽略');
            return;
        }
        if (!payload.session_id || !payload.message_id) {
            console.warn('[Proactive] 主动消息负载缺少 session_id / message_id，已忽略');
            return;
        }

        if (payload.activated === true) {
            _ensureAppended(payload);
            _locateSession(payload.session_id);
            return;
        }

        _refreshSessions();
        _ensureAppended(payload);
        _markReadIfActive(payload.session_id);
    }

    /**
     * 用户正在查看该会话时标记已读（静默失败降级：不阻塞消息展示）。
     *
     * 参数:
     * - `sessionId`: 消息所属会话 UUID。
     */
    function _markReadIfActive(sessionId) {
        if (RamariaStore.get('activeSessionId') !== sessionId) return;
        if (!RamariaApi.session || typeof RamariaApi.session.markRead !== 'function') return;

        RamariaApi.session.markRead(sessionId).catch(function (err) {
            console.warn('[Proactive] 标记会话已读失败:', (err && err.message) || err);
        });
    }

    /**
     * 刷新会话列表（静默失败降级：不阻塞消息展示）。
     */
    function _refreshSessions() {
        RamariaApi.session.list().then(function (sessions) {
            RamariaStore.set('sessions', sessions || []);
        }).catch(function (err) {
            console.warn('[Proactive] 刷新会话列表失败:', (err && err.message) || err);
        });
    }

    /**
     * 当前会话匹配时把消息幂等追加到 Store（按 message_id 去重）。
     *
     * 参数:
     * - `payload`: 已校验的事件负载。
     */
    function _ensureAppended(payload) {
        if (RamariaStore.get('activeSessionId') !== payload.session_id) return;

        var messages = RamariaStore.get('messages') || [];
        for (var i = 0; i < messages.length; i++) {
            if (messages[i] && messages[i].id === payload.message_id) return;
        }

        RamariaStore.appendMessage({
            id: payload.message_id,
            role: 'assistant',
            content: payload.content || '',
            persona_uid: payload.persona || null,
            created_at: payload.created_at || Date.now(),
            is_proactive: true,
        });
    }

    /**
     * 切换到目标会话（通知点击定位）。
     *
     * 参数:
     * - `sessionId`: 目标会话 UUID。
     *
     * 说明:
     * - 已在对话视图：经 ChatView 打开该会话（内部含已打开跳过与流式保护）；
     * - 其它视图：经 Router 切到对话视图并携带 sessionId，由 chat 视图 enter 钩子加载。
     */
    function _locateSession(sessionId) {
        if (RamariaRouter.getCurrentView() === 'chat') {
            if (typeof RamariaChatView !== 'undefined' &&
                RamariaChatView && typeof RamariaChatView.openSession === 'function') {
                RamariaChatView.openSession(sessionId);
            }
            return;
        }
        RamariaRouter.showView('chat', { sessionId: sessionId, fromView: 'proactive' });
    }

    // =========================================================
    // 公开 API
    // =========================================================

    return {
        /** 初始化事件监听（应用启动时调用一次） */
        init: init,
        /** 处理一条主动消息事件负载（可独立测试） */
        handlePayload: handlePayload,
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaProactive', {
    value: RamariaProactive,
    writable: false,
    configurable: false,
});
