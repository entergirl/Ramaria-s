/**
 * js/views/chat.js — Ramaria 对话视图
 *
 * 职责:
 * - 对话主界面：消息列表 + 流式渲染 + 输入框 + SessionBar + PersonaSelector
 * - 注册 Router enter/leave 钩子：enter 时加载会话和人格、leave 时清理事件监听
 * - 发送消息流程：追加用户消息 → 调用 API → 创建流式气泡 → 监听 chat-delta/chat-done/chat-error
 * - 会话管理：新建/切换/删除，自动保存和恢复
 *
 * 设计特点:
 * - 所有 Tauri Event 监听在 enter 时注册、leave 时注销，防止事件泄漏
 * - 流式追加使用 RamariaMessageBubble.updateContent，打字光标 CSS 驱动
 * - PersonaSelector 下拉联动 Store.personas，默认选中 rama-0001
 * - 空状态显示引导文案和快捷提示词
 * - 双层刷新策略：rAF（16ms/帧） + maxBatchTimer（32ms 安全网）防止标签页后台卡顿
 * - scrollToBottom rAF 批量化（单帧内多次调用合并为一次）
 * - 流式期间 GPU 层提示 + content-visibility:auto 加速渲染
 * - Enter 发送 / Shift+Enter 换行
 *
 * 依赖:
 * - RamariaApi / RamariaStore / RamariaRouter
 * - RamariaMessageBubble（js/components/message-bubble.js）
 * - RamariaToast（js/components/toast.js）
 * - RamariaFormat（js/utils/format.js）
 * - TauriBridge（js/tauri-bridge.js）
 * - CSS: css/views/chat.css + css/animations.css
 */

var RamariaChatView = (function () {
    'use strict';

    // =========================================================
    // 内部状态
    // =========================================================

    /** 取消 Tauri 事件监听的函数列表 */
    var _unlistenFns = [];
    /** Router 钩子取消注册函数列表 */
    var _unregisterFns = [];
    /** Store 订阅取消函数 */
    var _unsubs = [];

    /**
     * 消息自增计数器（用于防止同一毫秒内多条消息 ID 碰撞）。
     * Date.now() 在人类交互场景下几乎不可能碰撞，但作为防御性编程，
     * 计数器确保即使极端情况（脚本批量发送）下每条消息 ID 唯一。
     */
    var _msgCounter = 0;

    /** 流式追加文本缓冲（用于 rAF 批量更新） */
    var _pendingDelta = '';
    /** 流式消息 ID */
    var _streamingMsgId = null;
    /** rAF 句柄（16ms 一帧，与显示器刷新率同步） */
    var _rafHandle = null;
    /**
     * 最大批量间隔定时器句柄（安全网）。
     *
     * rAF 在标签页不可见时可能被浏览器节流（降到 1fps 甚至暂停），
     * 纯 rAF 方案在后台标签页中可能导致 delta 堆积。
     * 此定时器作为安全网：无论 rAF 是否触发，最多 32ms（2帧）必须刷新。
     */
    var _maxBatchTimer = null;
    /** 最大批量间隔（毫秒）。32ms ≈ 2 帧 @60Hz，足够快且不造成 jank */
    var MAX_BATCH_MS = 32;
    /** 滚动操作防抖 rAF 句柄 */
    var _scrollRafHandle = null;

    // =========================================================
    // DOM 快捷查询
    // =========================================================

    function $(id) { return document.getElementById(id); }

    /** 获取对话视图中的消息列表容器 */
    function _msgListEl() {
        return document.querySelector('#view-chat .chat-message-list');
    }

    /** 获取输入框 */
    function _inputEl() {
        return document.querySelector('#view-chat .chat-input-textarea');
    }

    /** 获取发送按钮 */
    function _sendBtnEl() {
        return document.querySelector('#view-chat .chat-send-btn');
    }

    // =========================================================
    // 渲染
    // =========================================================

    function render() {
        var container = $('view-chat');
        if (!container) {
            console.error('[ChatView] 找不到 #view-chat 容器');
            return;
        }

        container.innerHTML = '';

        // ── SessionBar ──
        var sessionBar = document.createElement('div');
        sessionBar.className = 'chat-session-bar';
        sessionBar.id = 'chat-session-bar';
        sessionBar.innerHTML =
            '<button class="chat-session-new-btn" id="chat-session-new" title="新建会话" aria-label="新建会话">+</button>' +
            '<div class="flex gap-1" id="chat-session-tabs" style="display:flex;gap:var(--space-1);overflow-x:auto;"></div>';
        container.appendChild(sessionBar);

        // ── 消息列表 ──
        var msgList = document.createElement('div');
        msgList.className = 'chat-message-list';
        msgList.id = 'chat-message-list';
        msgList.setAttribute('role', 'log');              // 动态消息区域
        msgList.setAttribute('aria-live', 'polite');      // 屏幕阅读器：新消息时朗读
        msgList.setAttribute('aria-label', '对话消息列表');
        msgList.setAttribute('tabindex', '0');            // 可聚焦以支持键盘滚动
        container.appendChild(msgList);

        // 初始空状态
        _showEmptyState(msgList);

        // ── 输入区域 ──
        var inputArea = document.createElement('div');
        inputArea.className = 'chat-input-area';
        inputArea.id = 'chat-input-area';
        inputArea.innerHTML =
            '<div class="chat-input-row">' +
                '<textarea class="chat-input-textarea" id="chat-input" ' +
                    'placeholder="输入消息... (Enter 发送 · Shift+Enter 换行)" rows="1" aria-label="消息输入框"></textarea>' +
                '<button class="chat-send-btn" id="chat-send-btn" aria-label="发送消息" title="发送 (Enter)">' +
                    '↑' +
                '</button>' +
            '</div>' +
            '<div class="chat-input-toolbar">' +
                '<label style="font-size:11px;color:var(--text-tertiary);">对话人格</label>' +
                '<select id="chat-persona-select" aria-label="选择对话人格">' +
                    '<option value="rama-0001">默认 (rama-0001)</option>' +
                '</select>' +
            '</div>' +
            '<div class="chat-streaming-hint hidden" id="chat-streaming-hint">' +
                '<span class="chat-streaming-dot"></span>' +
                '<span class="chat-streaming-dot"></span>' +
                '<span class="chat-streaming-dot"></span>' +
                ' 正在生成回复...' +
            '</div>';
        container.appendChild(inputArea);

        // ── 事件绑定 ──
        _bindInputEvents();
        _bindSessionEvents();
    }

    /** 空状态展示 */
    function _showEmptyState(msgList) {
        if (!msgList) msgList = _msgListEl();
        if (!msgList) return;
        msgList.innerHTML =
            '<div class="chat-empty-state">' +
                '<div class="chat-empty-icon" aria-hidden="true">🪸</div>' +
                '<div class="chat-empty-title">开始一段对话</div>' +
                '<div class="chat-empty-hint">' +
                    'Ramaria 会记住你们的对话，并从中了解你的性格和偏好。<br>' +
                    '聊得越多，它越懂你。' +
                '</div>' +
                '<div class="chat-empty-prompts">' +
                    '<button class="chat-empty-prompt" data-prompt="你好，介绍一下你自己吧">👋 自我介绍</button>' +
                    '<button class="chat-empty-prompt" data-prompt="今天心情不太好，想聊聊天">💭 聊聊心情</button>' +
                    '<button class="chat-empty-prompt" data-prompt="我有个问题想请教你...">❓ 请教问题</button>' +
                '</div>' +
            '</div>';

        // 点击快捷提示词
        var prompts = msgList.querySelectorAll('.chat-empty-prompt');
        for (var i = 0; i < prompts.length; i++) {
            prompts[i].addEventListener('click', function () {
                var prompt = this.getAttribute('data-prompt');
                if (prompt) {
                    var input = _inputEl();
                    if (input) {
                        input.value = prompt;
                        input.focus();
                    }
                }
            });
        }
    }

    // =========================================================
    // 输入框事件
    // =========================================================

    function _bindInputEvents() {
        var input = _inputEl();
        var sendBtn = _sendBtnEl();

        if (!input || !sendBtn) return;

        // Enter 发送 / Shift+Enter 换行
        input.addEventListener('keydown', function (e) {
            if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                _handleSend();
            }
        });

        // 自动调整高度，空内容时回退到 min-height
        input.addEventListener('input', function () {
            if (!this.value) {
                this.style.height = '36px';
                return;
            }
            this.style.height = 'auto';
            this.style.height = Math.min(this.scrollHeight, 120) + 'px';
        });

        // 发送按钮
        sendBtn.addEventListener('click', _handleSend);
    }

    // =========================================================
    // SessionBar 事件
    // =========================================================

    function _bindSessionEvents() {
        var newBtn = $('chat-session-new');
        if (newBtn) {
            newBtn.addEventListener('click', _handleNewSession);
        }
    }

    function _renderSessionTabs() {
        var tabsContainer = $('chat-session-tabs');
        if (!tabsContainer) return;

        var sessions = RamariaStore.get('sessions') || [];
        var activeId = RamariaStore.get('activeSessionId');

        tabsContainer.innerHTML = '';

        // 倒序（最新在前）
        for (var i = sessions.length - 1; i >= 0; i--) {
            var s = sessions[i];

            var tab = document.createElement('button');
            tab.className = 'chat-session-tab';
            if (s.id === activeId) tab.classList.add('active');
            tab.setAttribute('data-session-id', s.id);

            // 智能标题：第一条消息的前20字，或时间
            var title = s.id ? s.id.substring(0, 8) : '新会话';
            if (s.message_count > 0) {
                title = '会话 #' + (i + 1);
            }
            tab.textContent = title;
            tab.title = '会话: ' + (s.id ? s.id.substring(0, 8) + '...' : '未知') +
                        ' | 消息数: ' + (s.message_count || 0);

            tab.addEventListener('click', function () {
                var sid = this.getAttribute('data-session-id');
                _switchSession(sid);
            });

            // 右键删除
            tab.addEventListener('contextmenu', function (e) {
                e.preventDefault();
                var sid = this.getAttribute('data-session-id');
                RamariaModal.show({
                    title: '删除会话',
                    body: '<p style="font-size:13px;color:var(--text-secondary);">确定要删除此会话及其所有消息吗？此操作不可撤销。</p>',
                    footer: '<button class="btn btn-secondary" data-action="cancel">取消</button>' +
                            '<button class="btn btn-primary" style="background:var(--pink-500);" data-action="delete">删除</button>',
                    onAction: function (action) {
                        if (action === 'delete') {
                            _deleteSession(sid);
                        }
                    },
                });
            });

            tabsContainer.appendChild(tab);
        }
    }

    // =========================================================
    // 会话操作
    // =========================================================

    async function _handleNewSession() {
        // 守卫：当前会话为空时不创建新会话
        var msgs = RamariaStore.get('messages') || [];
        if (msgs.length === 0) {
            RamariaToast.show('info', '当前对话为空', '无需新建会话，直接开始输入即可');
            return;
        }

        try {
            var session = await RamariaApi.session.create();
            RamariaStore.set('activeSessionId', session.id);
            RamariaStore.set('messages', []);

            // 刷新会话列表
            var sessions = await RamariaApi.session.list();
            RamariaStore.set('sessions', sessions);

            _renderSessionTabs();
            _clearMessages();

            RamariaToast.show('success', '新会话已创建');
        } catch (err) {
            console.error('[ChatView] 创建会话失败:', err);
            RamariaToast.show('error', '创建会话失败', err.message || '未知错误');
        }
    }

    async function _switchSession(sessionId) {
        if (!sessionId) return;

        try {
            RamariaRouter.setSessionInfo('加载中...');
            var session = await RamariaApi.session.get(sessionId);

            RamariaStore.set('activeSessionId', sessionId);
            RamariaStore.set('messages', session.messages || []);
            _renderSessionTabs();
            _renderAllMessages();

            RamariaRouter.setSessionInfo('会话: ' + sessionId.substring(0, 8) + '...');
        } catch (err) {
            console.error('[ChatView] 切换会话失败:', err);
            RamariaToast.show('error', '切换会话失败', err.message || '未知错误');
            RamariaRouter.setSessionInfo('');
        }
    }

    async function _deleteSession(sessionId) {
        if (!sessionId) return;

        try {
            await RamariaApi.session.delete(sessionId);

            var activeId = RamariaStore.get('activeSessionId');
            if (activeId === sessionId) {
                RamariaStore.set('activeSessionId', null);
                RamariaStore.set('messages', []);
            }

            // 刷新列表
            var sessions = await RamariaApi.session.list();
            RamariaStore.set('sessions', sessions);
            _renderSessionTabs();

            if (activeId === sessionId) {
                _clearMessages();
            }

            RamariaToast.show('success', '会话已删除');
        } catch (err) {
            console.error('[ChatView] 删除会话失败:', err);
            RamariaToast.show('error', '删除失败', err.message || '未知错误');
        }
    }

    // =========================================================
    // 发送消息
    // =========================================================

    async function _handleSend() {
        var input = _inputEl();
        if (!input) return;

        var text = input.value.trim();
        if (!text) return;

        // 检查流式状态
        if (RamariaStore.get('isStreaming')) {
            RamariaToast.show('warning', '请等待当前回复完成');
            return;
        }

        // 检查应用状态
        var appState = RamariaStore.get('appState');
        if (appState !== 'ready' && appState !== 'degraded') {
            RamariaToast.show('warning', '应用尚未就绪');
            return;
        }

        // 清空输入框并重置高度到最小值，防止滚动条残留
        input.value = '';
        input.style.height = '36px';

        // 禁用输入
        _setInputEnabled(false);

        // 确保有活跃会话
        var sessionId = RamariaStore.get('activeSessionId');
        if (!sessionId) {
            try {
                var session = await RamariaApi.session.create();
                sessionId = session.id;
                RamariaStore.set('activeSessionId', sessionId);

                var sessions = await RamariaApi.session.list();
                RamariaStore.set('sessions', sessions);
                _renderSessionTabs();
            } catch (err) {
                console.error('[ChatView] 自动创建会话失败:', err);
                RamariaToast.show('error', '创建会话失败', '无法自动创建会话');
                _setInputEnabled(true);
                return;
            }
        }

        // 当前人格
        var personaSelect = $('chat-persona-select');
        var personaUid = personaSelect ? personaSelect.value : 'rama-0001';

        // 生成用户消息 ID（时间戳 + 自增计数器防碰撞）
        var now = Date.now();
        _msgCounter++;
        var userMsgId = 'msg-' + now + '-' + _msgCounter + '-u';

        // 1. 追加用户消息到 Store（Store.appendMessage 触发订阅者 → _renderAllMessages 全量渲染，
        //    不需要额外调用 _appendBubble，否则会重复插入第二条气泡）
        RamariaStore.appendMessage({
            id: userMsgId,
            role: 'user',
            content: text,
            persona_uid: personaUid,
            created_at: now,
        });

        // 2. 自动滚动到底部
        _scrollToBottom();

        // 4. 发送 API 请求
        try {
            var result = await RamariaApi.chat.send(text, personaUid, sessionId);
            var requestId = result.request_id;

            console.log('[ChatView] 消息已发送, request_id: ' + requestId);

            // 5. 设置流式状态
            RamariaStore.set('isStreaming', true);
            RamariaStore.set('streamingRequestId', requestId);

            // 6. 启用流式性能优化（GPU 层 + content-visibility）
            _enableStreamOptimizations();

            // 7. 创建流式气泡
            var streamingId = 'msg-' + requestId;
            _streamingMsgId = streamingId;
            _pendingDelta = '';

            var bubble = RamariaMessageBubble.createStreaming({
                id: streamingId,
                role: 'assistant',
            });
            var msgList = _msgListEl();
            if (msgList) msgList.appendChild(bubble);
            _scrollToBottom();

            // 7. 显示流式提示
            var hint = $('chat-streaming-hint');
            if (hint) hint.classList.remove('hidden');

        } catch (err) {
            console.error('[ChatView] 发送消息失败:', err);
            RamariaToast.show('error', '发送失败', err.message || '未知错误');
            _setInputEnabled(true);
        }
    }

    // =========================================================
    // 流式事件监听
    // =========================================================

    function _listenStreamEvents() {
        // 检查 Tauri 可用性
        if (!TauriBridge || !TauriBridge.isTauri || !TauriBridge.isTauri()) {
            console.warn('[ChatView] 非 Tauri 环境，跳过流式事件监听');
            return;
        }

        // chat-delta（Rust 字段名: content）
        TauriBridge.listen('chat-delta', function (event) {
            var payload = event.payload;
            if (!payload || !payload.request_id) return;

            var reqId = RamariaStore.get('streamingRequestId');
            if (payload.request_id !== reqId) return;

            // Rust ChatDeltaPayload 字段为 content，不是 content_delta/delta
            var delta = payload.content || '';
            if (!delta) return;

            _pendingDelta += delta;

            /*
             * 双层刷新策略：
             *
             * 第 1 层：rAF（16ms 一帧 @60Hz）
             *   与显示器刷新率同步，在 GPU 垂直同步间隙批量提交 DOM 更新。
             *   这是主力机制——高频 delta（每 5-10ms 一条）会被合并到一帧。
             *
             * 第 2 层：maxBatchTimer（32ms 安全网）
             *   防止 rAF 节流导致文本长时间不显示。
             *   场景：标签页后台（rAF 降到 1fps）、极慢流（delta 间隔 >16ms）。
             *   两个定时器互斥：任一触发后清除另一个。
             */

            // 第 1 层：rAF
            if (!_rafHandle) {
                _rafHandle = requestAnimationFrame(_flushDelta);
            }

            // 第 2 层：max batch timer（安全网）
            if (!_maxBatchTimer) {
                _maxBatchTimer = setTimeout(function () {
                    // 如果 rAF 还没触发，强制刷新
                    if (_rafHandle) {
                        cancelAnimationFrame(_rafHandle);
                        _rafHandle = null;
                    }
                    _maxBatchTimer = null;
                    _flushDelta();
                }, MAX_BATCH_MS);
            }
        }).then(function (unlisten) {
            _unlistenFns.push(unlisten);
        }).catch(function (err) {
            console.error('[ChatView] chat-delta 监听注册失败:', err);
        });

        // chat-done（Rust 字段: request_id, backend_id, total_chars，无 content；
        //   完整内容已通过 chat-delta 送达 DOM，此处读 DOM 文本作为 finalContent）
        TauriBridge.listen('chat-done', function (event) {
            var payload = event.payload;
            if (!payload || !payload.request_id) return;

            var reqId = RamariaStore.get('streamingRequestId');
            if (payload.request_id !== reqId) return;

            console.log('[ChatView] 流式完成: ' + payload.request_id);

            // 强制刷新所有待处理 delta（清空 rAF + maxBatchTimer）
            _flushDelta();

            // 卸载流式性能优化
            _disableStreamOptimizations();

            var completedMsgId = _streamingMsgId;  // 在置空前保存

            // 从 DOM 读取已累积的完整文本（chat-delta 已逐字写入 .msg-bubble-text）
            var finalContent = '';
            if (completedMsgId) {
                var textEl = document.querySelector(
                    '.msg-bubble-wrapper[data-message-id="' + completedMsgId + '"] .msg-bubble-text'
                );
                if (textEl) finalContent = textEl.textContent || '';
            }

            var createdAt = Date.now();
            RamariaMessageBubble.finalize(completedMsgId, finalContent, createdAt);

            // ★ 先重置 isStreaming，再 appendMessage，确保订阅者能正常渲染
            RamariaStore.set('isStreaming', false);
            RamariaStore.set('streamingRequestId', null);
            _streamingMsgId = null;
            _pendingDelta = '';

            // 追加助手消息到 Store
            RamariaStore.appendMessage({
                id: completedMsgId,
                role: 'assistant',
                content: finalContent,
                persona_uid: '',
                created_at: createdAt,
            });

            // 隐藏流式提示
            var hint = $('chat-streaming-hint');
            if (hint) hint.classList.add('hidden');

            // 重新启用输入
            _setInputEnabled(true);

            // 刷新会话列表（消息数已更新）
            RamariaApi.session.list().then(function (sessions) {
                RamariaStore.set('sessions', sessions);
                _renderSessionTabs();
            }).catch(function () { /* ignore */ });

            _scrollToBottom();
        }).then(function (unlisten) {
            _unlistenFns.push(unlisten);
        }).catch(function (err) {
            console.error('[ChatView] chat-done 监听注册失败:', err);
        });

        // chat-error（Rust 字段: error_title, error_detail, retryable）
        TauriBridge.listen('chat-error', function (event) {
            var payload = event.payload;
            if (!payload || !payload.request_id) return;

            var reqId = RamariaStore.get('streamingRequestId');
            if (payload.request_id !== reqId) return;

            // Rust ChatErrorPayload 字段为 error_title / error_detail，没有 error
            var title = payload.error_title || '生成失败';
            var detail = payload.error_detail || '请稍后重试';
            console.error('[ChatView] 流式错误: ' + payload.request_id, title, detail);

            // 强制刷新所有待处理 delta 后清理
            _flushDelta();

            // 卸载流式性能优化
            _disableStreamOptimizations();

            // 标记气泡错误
            RamariaMessageBubble.markError(_streamingMsgId, title);

            // 重置状态
            RamariaStore.set('isStreaming', false);
            RamariaStore.set('streamingRequestId', null);
            _streamingMsgId = null;

            // 隐藏流式提示
            var hint = $('chat-streaming-hint');
            if (hint) hint.classList.add('hidden');

            // 重新启用输入
            _setInputEnabled(true);

            RamariaToast.show('error', title, detail);
        }).then(function (unlisten) {
            _unlistenFns.push(unlisten);
        }).catch(function (err) {
            console.error('[ChatView] chat-error 监听注册失败:', err);
        });
    }

    /** 清理 Tauri 事件监听 */
    function _unlistenAll() {
        for (var i = 0; i < _unlistenFns.length; i++) {
            try { _unlistenFns[i](); } catch (_) { /* ignore */ }
        }
        _unlistenFns = [];

        // 清理 rAF 和 maxBatchTimer
        if (_rafHandle) {
            cancelAnimationFrame(_rafHandle);
            _rafHandle = null;
        }
        if (_maxBatchTimer) {
            clearTimeout(_maxBatchTimer);
            _maxBatchTimer = null;
        }
        if (_scrollRafHandle) {
            cancelAnimationFrame(_scrollRafHandle);
            _scrollRafHandle = null;
        }

        // 卸载流式优化
        _disableStreamOptimizations();

        _pendingDelta = '';
        _streamingMsgId = null;
    }

    // =========================================================
    // 消息渲染
    // =========================================================

    function _appendBubble(msg) {
        var msgList = _msgListEl();
        if (!msgList) return;

        // 移除空状态
        var emptyState = msgList.querySelector('.chat-empty-state');
        if (emptyState) emptyState.remove();

        var bubble = RamariaMessageBubble.create(msg);
        msgList.appendChild(bubble);
        _scrollToBottom();
    }

    function _renderAllMessages() {
        var msgList = _msgListEl();
        if (!msgList) return;

        var messages = RamariaStore.get('messages') || [];

        // 移除空状态和现有气泡（保留空状态类元素以外的所有气泡）
        var bubbles = msgList.querySelectorAll('.msg-bubble-wrapper');
        for (var i = 0; i < bubbles.length; i++) {
            bubbles[i].remove();
        }

        var emptyState = msgList.querySelector('.chat-empty-state');
        if (messages.length === 0) {
            if (!emptyState) _showEmptyState(msgList);
            return;
        }

        if (emptyState) emptyState.remove();

        for (var j = 0; j < messages.length; j++) {
            var bubble = RamariaMessageBubble.create(messages[j]);
            msgList.appendChild(bubble);
        }

        _scrollToBottom();
    }

    function _clearMessages() {
        var msgList = _msgListEl();
        if (!msgList) return;
        msgList.innerHTML = '';
        _showEmptyState(msgList);
    }

    // =========================================================
    // 流式辅助
    // =========================================================

    /**
     * 刷新待处理的 delta 到 DOM（rAF / maxBatchTimer 双路径调用）。
     *
     * 说明:
     * - 由 rAF 回调和 maxBatchTimer 回调共享
     * - 清空 _pendingDelta 和两个定时器句柄
     * - 调用 RamariaMessageBubble.updateContent 写入 DOM
     * - 触发滚动（通过 rAF 批量化，避免 layout thrashing）
     */
    function _flushDelta() {
        // 清除两个定时器
        if (_rafHandle) {
            cancelAnimationFrame(_rafHandle);
            _rafHandle = null;
        }
        if (_maxBatchTimer) {
            clearTimeout(_maxBatchTimer);
            _maxBatchTimer = null;
        }

        if (_streamingMsgId && _pendingDelta) {
            RamariaMessageBubble.updateContent(_streamingMsgId, _pendingDelta);
            _pendingDelta = '';
            _scrollToBottomBathed();
        }
    }

    // =========================================================
    // 辅助
    // =========================================================

    /**
     * 滚动到消息列表底部（rAF 批量化）。
     *
     * 说明:
     * - 在单帧内多次调用 _scrollToBottom 只执行最后一次（合并写操作）。
     * - 避免在高频 delta 场景下每 delta 都触发 layout→paint 循环。
     * - scrollTop 赋值触发同步 layout，必须限制频率。
     */
    function _scrollToBottomBathed() {
        if (_scrollRafHandle) return;  // 已有待处理的滚动，跳过
        _scrollRafHandle = requestAnimationFrame(function () {
            _scrollRafHandle = null;
            var msgList = _msgListEl();
            if (msgList) {
                msgList.scrollTop = msgList.scrollHeight;
            }
        });
    }

    /**
     * 立即滚动到底部（非流式场景：消息完成、切换会话等）。
     */
    function _scrollToBottom() {
        var msgList = _msgListEl();
        if (msgList) {
            requestAnimationFrame(function () {
                msgList.scrollTop = msgList.scrollHeight;
            });
        }
    }

    function _setInputEnabled(enabled) {
        var input = _inputEl();
        var sendBtn = _sendBtnEl();

        if (input) input.disabled = !enabled;
        if (sendBtn) sendBtn.disabled = !enabled;
    }

    /**
     * 流式开始时：添加 GPU 加速提示层（will-change）
     * 和 content-visibility 优化。
     */
    function _enableStreamOptimizations() {
        var msgList = _msgListEl();
        if (msgList) {
            // GPU 合成层提示：告知浏览器此区域将频繁重绘
            msgList.classList.add('gpu-layer');
            // content-visibility: auto 跳过屏幕外气泡渲染
            msgList.classList.add('content-optimized');
            // 标记流式状态供 CSS 使用
            msgList.classList.add('is-streaming');
        }
    }

    /**
     * 流式结束时：移除性能优化提示层。
     */
    function _disableStreamOptimizations() {
        var msgList = _msgListEl();
        if (msgList) {
            msgList.classList.remove('gpu-layer');
            msgList.classList.remove('content-optimized');
            msgList.classList.remove('is-streaming');
        }
    }

    // =========================================================
    // 人格选择器刷新
    // =========================================================

    async function _refreshPersonaSelector() {
        var select = $('chat-persona-select');
        if (!select) return;

        try {
            var personas = await RamariaApi.memory.getPersonas();
            RamariaStore.set('personas', personas || []);

            select.innerHTML = '';
            for (var i = 0; i < personas.length; i++) {
                var opt = document.createElement('option');
                opt.value = personas[i].uid;
                opt.textContent = personas[i].name + ' (' + personas[i].uid + ')';
                select.appendChild(opt);
            }

            // 默认选中 rama-0001
            var defaultOpt = select.querySelector('option[value="rama-0001"]');
            if (defaultOpt) select.value = 'rama-0001';
        } catch (err) {
            console.error('[ChatView] 加载人格列表失败:', err);
        }
    }

    // =========================================================
    // 生命周期
    // =========================================================

    function _registerHooks() {
        var unreg;

        unreg = RamariaRouter.registerHook('chat', 'enter', function () {
            console.log('[ChatView] enter');

            // 首次渲染
            render();

            // 加载数据
            _loadInitialData();

            // 注册流式事件监听
            _listenStreamEvents();

            // 订阅 messages 变更（用于外部触发渲染）
            var unsub = RamariaStore.subscribe('messages', function (newMsgs) {
                // 仅在非流式状态下全量重渲染（流式由 chat-delta 驱动）
                if (!RamariaStore.get('isStreaming')) {
                    _renderAllMessages();
                }
            });
            _unsubs.push(unsub);

            // 订阅会话变更
            unsub = RamariaStore.subscribe('activeSessionId', function (newId) {
                if (newId) {
                    RamariaRouter.setSessionInfo('会话: ' + newId.substring(0, 8) + '...');
                } else {
                    RamariaRouter.setSessionInfo('');
                }
            });
            _unsubs.push(unsub);

            // 更新内容区操作按钮
            RamariaRouter.setContentActions(
                '<button class="btn btn-ghost btn-sm" id="chat-clear-btn" title="清除当前会话消息">' +
                    '清空' +
                '</button>'
            );

            // 绑定清空按钮
            setTimeout(function () {
                var clearBtn = $('chat-clear-btn') || document.getElementById('chat-clear-btn');
                if (clearBtn) {
                    clearBtn.addEventListener('click', function () {
                        RamariaStore.set('messages', []);
                        _clearMessages();
                        RamariaToast.show('info', '消息展示已清空', '注意：存储中的消息未被删除');
                    });
                }
            }, 100);
        });
        _unregisterFns.push(unreg);

        unreg = RamariaRouter.registerHook('chat', 'leave', function () {
            console.log('[ChatView] leave');

            // 清理 Tauri 事件监听
            _unlistenAll();

            // 清理 Store 订阅
            for (var i = 0; i < _unsubs.length; i++) {
                try { _unsubs[i](); } catch (_) { /* ignore */ }
            }
            _unsubs = [];

            // 清理内容区操作按钮
            RamariaRouter.setContentActions('');
        });
        _unregisterFns.push(unreg);
    }

    async function _loadInitialData() {
        try {
            // 加载人格列表
            await _refreshPersonaSelector();

            // 加载会话列表
            var sessions = await RamariaApi.session.list();
            RamariaStore.set('sessions', sessions);
            _renderSessionTabs();

            // 如果有活跃会话，加载消息
            var activeId = RamariaStore.get('activeSessionId');
            if (activeId) {
                var session = await RamariaApi.session.get(activeId);
                RamariaStore.set('messages', session.messages || []);
                _renderAllMessages();
                RamariaRouter.setSessionInfo('会话: ' + activeId.substring(0, 8) + '...');
            } else if (sessions && sessions.length > 0) {
                // 自动选择最新会话
                await _switchSession(sessions[sessions.length - 1].id);
            }
        } catch (err) {
            console.error('[ChatView] 加载初始数据失败:', err);
        }
    }

    // =========================================================
    // 初始化
    // =========================================================

    function init() {
        console.log('[ChatView] 初始化对话视图...');
        _registerHooks();
    }

    // =========================================================
    // 公开 API
    // =========================================================

    return {
        init: init,
        destroy: function () {
            _unlistenAll();
            for (var i = 0; i < _unregisterFns.length; i++) {
                try { _unregisterFns[i](); } catch (_) { /* ignore */ }
            }
            _unregisterFns = [];
            for (var j = 0; j < _unsubs.length; j++) {
                try { _unsubs[j](); } catch (_) { /* ignore */ }
            }
            _unsubs = [];
            console.log('[ChatView] 已销毁');
        },
    };
})();

// 自动初始化
(function _autoInit() {
    if (typeof RamariaRouter === 'undefined') {
        setTimeout(_autoInit, 50);
        return;
    }
    RamariaChatView.init();

    // 若当前视图已激活（Router 已提前路由但钩子尚未注册），强制重新进入
    var currentView = RamariaRouter.getCurrentView();
    if (currentView === 'chat') {
        setTimeout(function () {
            if (RamariaRouter.getCurrentView() === 'chat') {
                RamariaRouter.showView('chat', { forceReenter: true });
            }
        }, 10);
    }
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaChatView', {
    value: RamariaChatView,
    writable: false,
    configurable: false,
});
