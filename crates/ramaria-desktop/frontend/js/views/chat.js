/**
 * js/views/chat.js — Ramaria 对话视图
 *
 * 职责:
 * - 对话主界面：消息列表 + 流式渲染 + 输入框 + SessionBar + PersonaSelector
 * - 注册 Router enter/leave 钩子：enter 时加载会话和人格、leave 时清理事件监听
 * - 发送消息流程：追加用户消息 → 调用 API → 创建流式气泡 → 监听 chat-delta/chat-done/chat-error
 * - 会话管理：新建/切换/删除，自动保存和恢复
 * - : SessionBar 区分活跃/已关闭（绿色圆点 / 灰色时间戳）
 * - : 已关闭 session 只读模式（隐藏输入框 + "此对话已关闭"提示）
 * - : "保存对话"按钮（关闭 session → 不清屏 → 下次消息自动创建新 session）
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
 * - 人格独立会话——每个人格拥有独立 Session，切换人格=切换好友
 * - 人格会话映射持久化（Store.personaSessions → 后端 settings）
 * - 修复人格昵称/头像位置（助手消息=左侧对话人，用户消息=右侧自己）
 *
 * 依赖:
 * - RamariaApi / RamariaStore / RamariaRouter
 * - RamariaMessageBubble（js/components/message-bubble.js）
 * - RamariaProgressBar（js/components/progress-bar.js, .1.1）
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

 /**
  * ★ v1.2 M4-A: 当前会话绑定的 persona_uid（真相源来自后端 session.persona_uid）。
  * 与下拉框选择的 currentPersonaUid 区别：
  * - currentPersonaUid: 用户通过 UI 选择的目标人格
  * - sessionPersonaUid: 后端 DB session 表中实际记录的 persona_uid
  * 正常情况下两者一致；不一致时以后端为准。
  * 此变量用于 ChatView 内部快速访问，避免频繁调用 Store.get('sessionPersonaUid')。
  */
    var _sessionPersonaUid = null;

    /**
     * ★ v1.2 M5-B: 标记是否通过 L1 卡片跳转加载了指定会话。
     * 若为 true，_loadInitialData 将跳过自动 persona 匹配和消息加载，
     * 避免覆盖已跳转加载的会话数据。
     */
    var _sessionJumped = false;

/** 取消 Tauri 事件监听的函数列表 */
    var _unlistenFns = [];
 /** Router 钩子取消注册函数列表 */
    var _unregisterFns = [];
 /** Store 订阅取消函数 */
    var _unsubs = [];

 /**
 * 消息自增计数器（用于防止同一毫秒内多条消息 ID 碰撞）。
 * Date.now 在人类交互场景下几乎不可能碰撞，但作为防御性编程，
 * 计数器确保即使极端情况（脚本批量发送）下每条消息 ID 唯一。
 */
    var _msgCounter = 0;

 /** 流式追加文本缓冲（用于 rAF 批量更新） */
    var _pendingDelta = '';
 /**
 * _pendingDelta 缓冲区最大字节数。
 *
 * 当浏览器标签页被后台挂起时，rAF 回调可能长时间不触发，
 * 导致 _pendingDelta 持续累积。此上限防止极端场景下内存无限增长
 * （如用户切换到其他标签页后长时间不返回）。
 *
 * 超过上限时强制刷新，无论 rAF 是否触发。
 */
    var MAX_PENDING_DELTA_BYTES = 10240; // 10 KB
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

 // ── 聊天页眉（Persona 头像 + 名称 + 保存按钮）──
 // 替代旧 SessionBar（tab 并行切换），改为社交平台风格页眉
 // 左：当前对话人格头像 + 名称 + 状态指示 右：保存对话按钮
 // 初始值不再硬编码 "Rama"，改为空字符串；
 // _loadInitialData 完成 persona 加载后由 _updateHeaderPersona 动态填充。
        var header = document.createElement('div');
        header.className = 'chat-header';
        header.id = 'chat-header';
        header.innerHTML =
            '<div class="chat-header-left">' +
                '<div class="chat-header-avatar" id="chat-header-avatar" aria-hidden="true"></div>' +
                '<div class="chat-header-info">' +
                    '<span class="chat-header-persona-name" id="chat-header-persona-name"></span>' +
                    '<span class="session-status-dot active" id="chat-header-status" title="对话中"></span>' +
                '</div>' +
            '</div>' +
            '<div class="chat-header-right">' +
                '<button class="btn btn-ghost btn-sm" id="chat-history-btn" title="查看会话历史" aria-label="历史会话">' +
                    '📋 历史' +
                '</button>' +
                '<button class="btn btn-ghost btn-sm" id="chat-save-btn" title="保存当前对话（关闭 session 并生成 L1 摘要）" aria-label="保存对话">' +
                    '💾 保存对话' +
                '</button>' +
            '</div>';
        container.appendChild(header);

 // ── 消息列表 ──
        var msgList = document.createElement('div');
        msgList.className = 'chat-message-list';
        msgList.id = 'chat-message-list';
        msgList.setAttribute('role', 'log');
        msgList.setAttribute('aria-live', 'polite');
        msgList.setAttribute('aria-label', '对话消息列表');
        msgList.setAttribute('tabindex', '0');
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
                '<label class="chat-input-persona-label">对话人格</label>' +
                '<select id="chat-persona-select" aria-label="选择对话人格">' +
                    '<option value="rama-0001">默认 (rama-0001)</option>' +
                '</select>' +
            '</div>' +
 // 已关闭 session 的只读提示
            '<div class="chat-readonly-banner hidden" id="chat-readonly-banner">' +
                '<span>🔒 此对话已关闭</span>' +
                '<button class="btn btn-primary btn-sm" id="chat-new-session-btn">开始新对话</button>' +
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

 // 人格选择器变更 → 切换人格会话（独立对话栏）
        var personaSelect = $('chat-persona-select');
        if (personaSelect) {
            personaSelect.addEventListener('change', _handlePersonaChange);
        }
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
 // 页眉事件（重构：SessionBar → Persona 页眉）
 // =========================================================

    function _bindSessionEvents() {
 // 保存对话按钮
        var saveBtn = $('chat-save-btn');
        if (saveBtn) {
            saveBtn.addEventListener('click', _handleSaveSession);
        }

 // 已关闭 session 只读模式下的"开始新对话"按钮
        var newSessionBtn = $('chat-new-session-btn');
        if (newSessionBtn) {
            newSessionBtn.addEventListener('click', _handleNewSessionFromReadonly);
        }
    }

 /**
 * 同步页眉 persona 头像和名称。
 *
 * 对齐需求：页眉显示当前聊天对象头像+昵称，类似社交平台对话框。
 * 新增头像渲染（首字母圆形 + 背景色由 uid hash 决定）。
 */
    function _updateHeaderPersona() {
        var select = $('chat-persona-select');
        var nameEl = $('chat-header-persona-name');
        var avatarEl = $('chat-header-avatar');
        if (!select || !nameEl) return;

        var selectedOpt = select.options[select.selectedIndex];
        if (selectedOpt) {
 // 提取纯名称（去掉 "(uid)" 后缀）
            var fullText = selectedOpt.textContent || '';
            var name = fullText.split(' (')[0] || fullText;
            nameEl.textContent = name;

 // 更新头像（首字母圆形）
            if (avatarEl) {
                var initial = name.charAt(0).toUpperCase() || '?';
                avatarEl.textContent = initial;
 // 根据 uid hash 分配稳定颜色
                var uid = selectedOpt.value || '';
                avatarEl.style.backgroundColor = _avatarColor(uid);
            }
        }
    }

 /**
 * 根据 uid 生成稳定的头像背景色。
 *
 * 说明:
 * - 简单 hash 算法确保同一 uid 始终得到相同颜色。
 * - 使用 HSL 色调轮（0-360），饱和度/亮度固定，视觉柔和。
 */
    function _avatarColor(uid) {
        if (!uid) return 'hsl(220, 25%, 55%)';
        var hash = 0;
        for (var i = 0; i < uid.length; i++) {
            hash = uid.charCodeAt(i) + ((hash << 5) - hash);
        }
        var hue = Math.abs(hash) % 360;
        return 'hsl(' + hue + ', 45%, 55%)';
    }

 // =========================================================
 // 人格切换（独立对话栏）
 // =========================================================

 /**
 * 处理人格选择器变更——切换到目标人格的独立会话。
 *
 * 对齐需求：类似切换好友——每个人格拥有独立对话栏，
 * 切换人格时切换到对应会话，消息历史互相隔离。
 *
 * 流程:
 * 1. 获取当前和目标人格 UID。
 * 2. 如果相同则跳过。
 * 3. 查找目标人格的已有 session。
 * 4. 有则加载历史消息，无则显示空状态（下次发送自动创建）。
 * 5. 更新 Store.currentPersonaUid + 同步页眉。
 */
    async function _handlePersonaChange() {
        var select = $('chat-persona-select');
        if (!select) return;

        var newPersonaUid = select.value;
        var oldPersonaUid = RamariaStore.get('currentPersonaUid');

 // 相同人格不处理
        if (newPersonaUid === oldPersonaUid) return;

        console.log('[ChatView] 切换人格: ' + (oldPersonaUid || '(none)') + ' → ' + newPersonaUid);

 // 不能切换时正在流式接收
        if (RamariaStore.get('isStreaming')) {
            RamariaToast.show('warning', '请等待当前回复完成后再切换');
            select.value = oldPersonaUid || '';
            return;
        }

 // 切换到目标人格
        await _switchToPersona(newPersonaUid);
    }

 /**
 * ★ v1.2 M4-A: 切换到指定人格的会话。
 *
 * 与 v1.1 的核心差异:
 * - personaSessions 降级为性能缓存（不依赖其做归属判断）
 * - 加载 session 后以 `session.persona_uid`（后端 DB 真相源）验证归属
 * - 新增 `_sessionPersonaUid` 追踪，与 Store.sessionPersonaUid 同步
 * - 缓存与真相源不一致时自动修正（过期缓存不阻塞正常流程）
 *
 * 参数:
 * - `personaUid`: 目标人格 UID。
 * - `silent`: 可选，true 时跳过 Toast 提示（初始加载用）。
 */
    async function _switchToPersona(personaUid, silent) {
 // 1. 更新 Store 中的当前选中人格（前端视角）
        RamariaStore.set('currentPersonaUid', personaUid);

 // 2. 从性能缓存查找该人格的已有 session（仅作 hint，不依赖做真相判断）
        var cachedSessionId = RamariaStore.getPersonaSession(personaUid);
        var messagesLoaded = false;

        if (cachedSessionId) {
            try {
                var session = await RamariaApi.session.get(cachedSessionId);

 // ★ v1.2: 验证后端真相源——session.persona_uid 是否与目标人格匹配
                var dbPersonaUid = session ? (session.persona_uid || null) : null;

                if (dbPersonaUid === personaUid) {
 // 后端确认归属正确：session 确实属于该人格
                    _sessionPersonaUid = personaUid;
                    RamariaStore.set('sessionPersonaUid', personaUid);

                    if (session.messages && session.messages.length > 0) {
                        RamariaStore.set('activeSessionId', cachedSessionId);
                        RamariaStore.set('messages', session.messages);
                        _renderAllMessages();
                        messagesLoaded = true;
                        _setReadonlyMode(false);

                        console.log('[ChatView] 切换人格 ' + personaUid
                            + ' → session ' + cachedSessionId.substring(0, 8)
                            + ' (persona_uid 已由后端确认, ' + session.messages.length + ' 条消息)');
                    } else {
 // session 归属正确但无消息，留空等待用户输入
                        RamariaStore.set('activeSessionId', null);
                        RamariaStore.set('messages', []);
                        _clearMessages();
                        console.log('[ChatView] 切换人格 ' + personaUid
                            + ' → session ' + cachedSessionId.substring(0, 8)
                            + ' 归属正确但无消息');
                    }
                } else if (dbPersonaUid === null) {
 // ★ 存量兼容：session.persona_uid 为 NULL（v1.1 及以前创建的旧数据）
 // 此类 session 尚未被后端 persona 绑定逻辑更新，暂时信任缓存
                    _sessionPersonaUid = personaUid;
                    RamariaStore.set('sessionPersonaUid', personaUid);

                    if (session.messages && session.messages.length > 0) {
                        RamariaStore.set('activeSessionId', cachedSessionId);
                        RamariaStore.set('messages', session.messages);
                        _renderAllMessages();
                        messagesLoaded = true;
                        _setReadonlyMode(false);

                        console.log('[ChatView] 切换人格 ' + personaUid
                            + ' → session ' + cachedSessionId.substring(0, 8)
                            + ' (存量 session, persona_uid=NULL, '
                            + session.messages.length + ' 条消息)');
                    }
                } else {
 // persona_uid 不匹配：缓存已过期（后端可能已更新 session 归属）
 // 清除过期缓存，显示空状态——下次发送消息时后端自动创建正确的新 session
                    console.warn('[ChatView] 缓存过期: persona=' + personaUid
                        + ', 缓存 session=' + cachedSessionId.substring(0, 8)
                        + ', DB persona_uid=' + dbPersonaUid
                        + '。清除过期缓存，显示空状态。');

                    RamariaStore.set('activeSessionId', null);
                    RamariaStore.set('messages', []);
                    RamariaStore.set('sessionPersonaUid', null);
                    _sessionPersonaUid = null;
                    _clearMessages();

 // 从缓存中移除过期映射
                    var ps = Object.assign({}, RamariaStore.get('personaSessions'));
                    delete ps[personaUid];
                    RamariaStore.set('personaSessions', ps, true);
                }
            } catch (err) {
                console.warn('[ChatView] 加载人格会话失败:', err);
 // 网络/后端异常：不清除缓存（可能是临时故障），但不阻塞 UI
                RamariaStore.set('activeSessionId', null);
                RamariaStore.set('messages', []);
                RamariaStore.set('sessionPersonaUid', null);
                _sessionPersonaUid = null;
                _clearMessages();
            }
        } else {
 // 无缓存 hint：该人格尚无活跃 session
            console.log('[ChatView] 切换人格 ' + personaUid + ' → 无缓存 session，显示空状态');
            RamariaStore.set('sessionPersonaUid', null);
            _sessionPersonaUid = null;
        }

        if (!messagesLoaded) {
 // 无已有会话或消息，显示空状态
            RamariaStore.set('activeSessionId', null);
            RamariaStore.set('messages', []);
            _clearMessages();
            _setReadonlyMode(false);
        }

 // 3. 同步页眉（头像+名称）
        _updateHeaderPersona();

 // 4. 持久化缓存映射（非阻塞，失败仅 warn）
        await _persistPersonaSessions();

        if (!silent) {
            var personas = RamariaStore.get('personas') || [];
            var personaName = personaUid;
            for (var i = 0; i < personas.length; i++) {
                if (personas[i].uid === personaUid) {
                    personaName = personas[i].name;
                    break;
                }
            }
            RamariaToast.show('info', '已切换到「' + personaName + '」');
        }
    }

 // =========================================================
 // 人格会话映射持久化
 // =========================================================

 /**
 * ★ v1.2: 将 personaSessions 缓存映射持久化到后端 settings。
 *
 * 说明:
 * - personaSessions 仅作为性能缓存（非真相源），真相源在后端 sessions.persona_uid。
 * - 每次 session 创建/切换时调用，保存缓存以供冷启动恢复。
 * - 失败时仅打印 warn（非关键路径，缓存可在下次正常运行时重建）。
 */
 /**
 * ★ v1.2 修复: 清除指定人格的会话缓存映射（仅内存，不持久化）。
 *
 * 场景: 保存对话后，_handleSaveSession 清除了 activeSessionId，
 * 但 personaSessions 仍指向已关闭的 session。
 * 若此时用户退出再进入（或应用重启），_loadInitialData 会将已关闭
 * session 重新激活。清除映射确保不会再加载已关闭的 session。
 *
 * 不在此函数内持久化——防止与 _handleSend 的异步持久化产生竞态。
 * 持久化由调用方负责（_handleSaveSession 在调用后 await _persistPersonaSessions）。
 */
    function _clearPersonaSessionCache(personaUid) {
        if (!personaUid) return;
        var map = Object.assign({}, RamariaStore.get('personaSessions') || {});
        if (map[personaUid]) {
            delete map[personaUid];
            RamariaStore.set('personaSessions', map);
            console.log('[ChatView] 已清除 persona=' + personaUid + ' 的会话缓存（内存）');
        }
    }

    async function _persistPersonaSessions() {
        try {
            var map = RamariaStore.get('personaSessions') || {};
            var json = JSON.stringify(map);
            await RamariaApi.config.updateSetting('persona_sessions', json);
            console.log('[ChatView] persona_sessions 缓存已持久化（' + Object.keys(map).length + ' 条）');
        } catch (err) {
            console.warn('[ChatView] 持久化 persona_sessions 缓存失败:', err);
        }
    }

 /**
 * ★ v1.2: 从后端 settings 恢复 personaSessions 缓存映射。
 *
 * 说明:
 * - 仅在 cold start（personaSessions 为空）时从后端恢复缓存。
 * - 缓存非真相源——session 归属以 DB session.persona_uid 为准。
 * - 若内存中已有映射（应用运行期间已建立），跳过后端恢复以避免覆盖。
 * - 解析失败或不存在时回退到空缓存。
 */
    async function _restorePersonaSessions() {
 // ★ 修复: 仅在 cold start（personaSessions 为空）时从后端恢复。
 // 若内存中已有映射，说明应用运行期间已通过 _handleSend 建立过映射，
 // 后端 settings 可能因 fire-and-forget 持久化未完成而处于过期状态，
 // 强制覆盖会导致"离开再返回看不到当前 session 内容"的问题。
        var existingMap = RamariaStore.get('personaSessions');
        if (existingMap && Object.keys(existingMap).length > 0) {
            console.log('[ChatView] persona_sessions 已在内存中（' + Object.keys(existingMap).length + ' 条），跳过后端恢复');
            return;
        }

        try {
            var settings = await RamariaApi.config.getSettings();
            if (settings && Array.isArray(settings)) {
                for (var i = 0; i < settings.length; i++) {
                    if (settings[i].key === 'persona_sessions') {
                        var map = JSON.parse(settings[i].value || '{}');
                        RamariaStore.set('personaSessions', map, true); // silent 避免触发渲染
                        console.log('[ChatView] 已恢复 persona_sessions:', Object.keys(map).length + ' 条映射');
                        return;
                    }
                }
            }
        } catch (err) {
            console.warn('[ChatView] 恢复 persona_sessions 失败:', err);
        }
    }

 // =========================================================
 // 只读模式 + 保存对话
 // =========================================================

 /**
 * 设置对话视图的只读模式。
 *
 * 对齐 T-V11-0-011: 已关闭 session 时隐藏输入框，
 * 显示"此对话已关闭"提示和"开始新对话"按钮。
 */
    function _setReadonlyMode(isReadonly) {
        var inputArea = document.querySelector('#view-chat .chat-input-area');
        var readonlyBanner = $('chat-readonly-banner');
        var inputEl = _inputEl();
        var sendBtn = _sendBtnEl();

        if (isReadonly) {
 // 隐藏输入区域
            if (inputArea) inputArea.classList.add('hidden');
 // 显示只读提示
            if (readonlyBanner) readonlyBanner.classList.remove('hidden');
 // 禁用输入
            if (inputEl) inputEl.disabled = true;
            if (sendBtn) sendBtn.disabled = true;
        } else {
 // 恢复输入区域
            if (inputArea) inputArea.classList.remove('hidden');
 // 隐藏只读提示
            if (readonlyBanner) readonlyBanner.classList.add('hidden');
 // 恢复输入
            if (inputEl) inputEl.disabled = false;
            if (sendBtn) sendBtn.disabled = false;
        }
    }

 /**
 * 处理"保存对话"按钮点击。
 *
 * 对齐 T-V11-0-012 + Python `force_close_current_session`:
 * - 调用 save_current_session Tauri Command
 * - 不清屏（保留当前消息）
 * - 插入系统分隔线"── 对话已保存 ──"
 * - 清除 activeSessionId（下次消息自动创建新 session）
 */
    async function _handleSaveSession() {
        try {
 // ★ v1.2: 获取当前会话绑定的 persona UID——优先 sessionPersonaUid（后端真相源），
 // 回退下拉框选择值（前端 UI 状态）
            var personaSelect = $('chat-persona-select');
            var personaUid = _sessionPersonaUid || (personaSelect ? personaSelect.value : null);
            if (!personaUid) {
                console.warn('[ChatView] 保存对话时无法确定 persona_uid');
            }

 // ★ 关键修复: 在 await 之前立即清除 activeSessionId + 禁用输入。
 // 否则 await RamariaApi.chat.save 期间 JavaScript 事件循环可被其他事件中断，
 // 用户若在此窗口内按下 Enter → _handleSend 会读到旧 session ID → 后端拒绝"会话已关闭"。
 // 将 clear 操作前置到异步 I/O 之前，消除竞态窗口。
            RamariaStore.set('activeSessionId', null);
            _setInputEnabled(false);

 // 调用后端保存（传入 persona_uid，确保 L1 摘要可被记忆页面查询）
            var result = await RamariaApi.chat.save(personaUid);

 // 保存完成后恢复输入
            _setInputEnabled(true);

 // 解析返回值：{ status: "ok"|"no_active_session", l1_generated: bool, session_id: string }
            var parsed;
            try {
                parsed = typeof result === 'string' ? JSON.parse(result) : result;
            } catch (_) {
                parsed = { status: 'ok', l1_generated: false };
            }

            var savedSessionId = parsed.session_id || null;

 // 仅在成功关闭 session 时插入系统分隔线（no_active_session 时不插入）
            if (parsed.status === 'ok') {
                var msgList = _msgListEl();
                if (msgList) {
                    var separator = document.createElement('div');
                    separator.className = 'chat-system-separator';
                    separator.textContent = '── 对话已保存 ──';
                    msgList.appendChild(separator);
                }
            }

 // ★ v1.2: 不清屏——保留当前消息。
 // ★ 修复: 清除 personaSessions 缓存映射。否则已关闭的 session 会被
 // _loadInitialData 重新激活（尤其是在应用重启/热重载时，
 // _restorePersonaSessions 从后端恢复旧映射导致加载已关闭 session）。
 // 已保存消息仍在 DOM 中可见；下次发送消息时 _handleSend 自动创建新 session。
 // _sessionPersonaUid 保留——用户仍在与同一 persona 对话。
 _clearPersonaSessionCache(personaUid);
 // 持久化清除后的映射到后端（非阻塞，失败仅 warn）
 _persistPersonaSessions().catch(function(err) {
     console.warn('[ChatView] 保存后持久化缓存失败:', err);
 });

 // 根据 L1 生成结果给出不同提示
            if (parsed.l1_generated) {
                RamariaToast.show('success', '对话已保存',
                    'L1 摘要已生成（' + (parsed.l1_count || 1) + ' 条），可前往「记忆」页面查看');
            } else if (parsed.status === 'no_active_session') {
                RamariaToast.show('info', '无活跃对话');
            } else {
                var msgCount = parsed.msg_count || 0;
                var detail = 'L1 摘要生成失败（会话有 ' + msgCount + ' 条消息）。请确认 LLM 服务正常运行。';
                _showL1RetryToast(savedSessionId, personaUid, detail);
            }
        } catch (err) {
            console.error('[ChatView] 保存对话失败:', err);
 // 保存失败时恢复输入（activeSessionId 已在 await 前清除，无需回滚）
            _setInputEnabled(true);
            RamariaToast.show('error', '保存失败', err.message || '未知错误');
        }
    }

 /**
 * L1 生成失败时显示带"重试"按钮的 Toast。
 *
 * 参数:
 * - `sessionId`: 刚关闭的 session（用于重试）。
 * - `personaUid`: 当前人格。
 */
    function _showL1RetryToast(sessionId, personaUid, detailMsg) {
        if (!sessionId) {
            RamariaToast.show('error', '❌ L1 摘要失败',
                detailMsg || '请确认 LLM 服务正常运行');
            return;
        }

 // 使用 error 级别让用户注意到
        RamariaToast.show('error', '❌ L1 摘要生成失败', detailMsg || '');
 // 延迟追加重试按钮到 toast 容器
        setTimeout(function () {
            var toastContainer = document.querySelector('.ramaria-toast-container');
            if (toastContainer) {
                var btn = _createRetryButton(sessionId, personaUid);
                toastContainer.appendChild(btn);
            }
        }, 100);
    }

    function _createRetryButton(sessionId, personaUid) {
        var btn = document.createElement('button');
        btn.className = 'btn btn-sm msg-retry-btn';
        btn.textContent = '🔄 重试生成 L1';
        btn.addEventListener('click', function () {
            _retryL1Generation(sessionId, personaUid);
        });
        return btn;
    }

    async function _retryL1Generation(sessionId, personaUid) {
        try {
            var result = await RamariaApi.chat.generateL1(sessionId, personaUid);
            var parsed = typeof result === 'string' ? JSON.parse(result) : result;

            if (parsed && parsed.l1_generated) {
                RamariaToast.show('success', 'L1 摘要已生成',
                    '摘要: ' + (parsed.summary || '(空)').substring(0, 40) + '...');
            } else {
                RamariaToast.show('error', '重试失败',
                    parsed && parsed.reason === 'no_messages'
                        ? '该会话无消息，无法生成摘要'
                        : '请确认 LLM 服务正常运行');
            }
        } catch (err) {
            console.error('[ChatView] L1 记忆生成失败:', err);
            RamariaToast.show('error', '重试失败', err.message || '未知错误');
        }
    }

 /**
 * 处理"开始新对话"按钮点击（已关闭 session 只读模式下）。
 */
    async function _handleNewSessionFromReadonly() {
        _setReadonlyMode(false);

 // ★ v1.2: 清除当前消息、活跃 session 和会话 persona 绑定
 // personaSessions 缓存保留（下次发送消息时后端自动创建新 session 并写入 persona_uid）
        RamariaStore.set('messages', []);
        RamariaStore.set('activeSessionId', null);
        RamariaStore.set('sessionPersonaUid', null);
        _sessionPersonaUid = null;
        _clearMessages();
        _showEmptyState(_msgListEl());

        RamariaToast.show('info', '已就绪', '开始输入即可创建新对话');
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
 // 高度重置由 CSS 的 .chat-input-textarea { min-height:36px } 保证，不设内联 style

 // 禁用输入
        _setInputEnabled(false);

 // ★ v1.2: 当前人格——优先从 session 真相源读取，回退前端下拉框
        var personaSelect = $('chat-persona-select');
        var personaUid = personaSelect ? personaSelect.value : 'rama-0001';

 // 如果已有活跃 session 且其 persona_uid 已绑定，以 DB 为准
        if (_sessionPersonaUid) {
            personaUid = _sessionPersonaUid;
            console.log('[ChatView] 当前会话已绑定 persona_uid=' + personaUid + '（后端真相源）');
        }

 // 确保有活跃会话
        var sessionId = RamariaStore.get('activeSessionId');
        if (!sessionId) {
            try {
                var session = await RamariaApi.session.create();
                sessionId = session.id;
                RamariaStore.set('activeSessionId', sessionId);

 // ★ v1.2: 建立人格→会话缓存映射并持久化（非真相源，仅供下次切换加速查找）
                RamariaStore.setPersonaSession(personaUid, sessionId);
                await _persistPersonaSessions();

 // ★ v1.2: 新建 session 时同步 sessionPersonaUid（此时 session 刚创建，persona_uid 尚未写入 DB）
 // 后端 send_message 的 resolve_session 阶段会完成绑定；前端假设绑定成功。
                _sessionPersonaUid = personaUid;
                RamariaStore.set('sessionPersonaUid', personaUid);
            } catch (err) {
                console.error('[ChatView] 自动创建会话失败:', err);
                RamariaToast.show('error', '创建会话失败', '无法自动创建会话');
                _setInputEnabled(true);
                return;
            }
        } else {
 // session 已存在，确保 sessionPersonaUid 同步
            if (!_sessionPersonaUid) {
                _sessionPersonaUid = personaUid;
                RamariaStore.set('sessionPersonaUid', personaUid);
            }
        }

 // 保存当前人格 UID（供 chat-done 事件使用）
        RamariaStore.set('currentPersonaUid', personaUid);

 // 生成用户消息 ID（时间戳 + 自增计数器防碰撞）
        var now = Date.now();
        _msgCounter++;
        var userMsgId = 'msg-' + now + '-' + _msgCounter + '-u';

 // 1. 追加用户消息到 Store
 // ★ 修复: 用户消息不设置 persona_uid（发言人是用户自己，气泡在右侧）
 // persona_uid 仅用于助手消息，标识"谁在回复"（气泡在左侧）
        RamariaStore.appendMessage({
            id: userMsgId,
            role: 'user',
            content: text,
            persona_uid: null,
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

 // 上限保护：_pendingDelta 超过阈值时强制刷新，防止标签页后台时内存无限增长
            if (_pendingDelta.length > MAX_PENDING_DELTA_BYTES) {
 // 取消所有待处理的定时器，直接强制刷新
                if (_rafHandle) {
                    cancelAnimationFrame(_rafHandle);
                    _rafHandle = null;
                }
                if (_maxBatchTimer) {
                    clearTimeout(_maxBatchTimer);
                    _maxBatchTimer = null;
                }
                _flushDelta();
                return;
            }

 /*
 * 双层刷新策略：
 *
 * 第 1 层：rAF（16ms 一帧 @60Hz）
 * 与显示器刷新率同步，在 GPU 垂直同步间隙批量提交 DOM 更新。
 * 这是主力机制——高频 delta（每 5-10ms 一条）会被合并到一帧。
 *
 * 第 2 层：maxBatchTimer（32ms 安全网）
 * 防止 rAF 节流导致文本长时间不显示。
 * 场景：标签页后台（rAF 降到 1fps）、极慢流（delta 间隔 >16ms）。
 * 两个定时器互斥：任一触发后清除另一个。
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
            console.error('[ChatView] 注册 chat-delta 监听失败:', err);
        });

 // chat-done（Rust 字段: request_id, backend_id, total_chars，无 content；
 // 完整内容已通过 chat-delta 送达 DOM，此处读 DOM 文本作为 finalContent）
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
 // ★ 修复: 助手消息携带当前人格 UID，前端据此在左侧气泡显示"谁在回复"
            var currentPersona = RamariaStore.get('currentPersonaUid') || '';
            RamariaStore.appendMessage({
                id: completedMsgId,
                role: 'assistant',
                content: finalContent,
                persona_uid: currentPersona,
                created_at: createdAt,
            });

 // 隐藏流式提示
            var hint = $('chat-streaming-hint');
            if (hint) hint.classList.add('hidden');

 // 重新启用输入
            _setInputEnabled(true);

            _scrollToBottom();
        }).then(function (unlisten) {
            _unlistenFns.push(unlisten);
        }).catch(function (err) {
            console.error('[ChatView] 注册 chat-done 监听失败:', err);
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
            console.error('[ChatView] 注册 chat-error 监听失败:', err);
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

    /**
     * 刷新人格选择器下拉框。
     *
     * 参数:
     * - `silent`: 可选，true 时仅填充选项但不改变选中值（用于跳转后保持已选 persona）。
     */
    async function _refreshPersonaSelector(silent) {
        var select = $('chat-persona-select');
        if (!select) return;

        // ★ v1.2 M5-B: 保存当前选中值（silent 模式下保留）
        var previousValue = silent ? select.value : null;

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

            if (silent && previousValue) {
                // silent 模式：恢复之前的选中值（若该选项仍存在）
                var prevOpt = select.querySelector('option[value="' + previousValue + '"]');
                if (prevOpt) {
                    select.value = previousValue;
                } else {
                    // 之前的 persona 不在列表中时回退默认
                    var defaultOpt = select.querySelector('option[value="rama-0001"]');
                    if (defaultOpt) select.value = 'rama-0001';
                }
            } else {
                // 默认选中 rama-0001
                var defaultOpt = select.querySelector('option[value="rama-0001"]');
                if (defaultOpt) select.value = 'rama-0001';
            }
        } catch (err) {
            console.error('[ChatView] 加载人格列表失败:', err);
        }
    }

 // =========================================================
 // 生命周期
 // =========================================================

    function _registerHooks() {
        var unreg;

        // ★ v1.2 M5-B: enter 钩子接收 Router options（第二个参数）
        // options 可能包含 { sessionId, personaUid, fromView } 等跨视图传递参数
        unreg = RamariaRouter.registerHook('chat', 'enter', function (_viewName, options) {
            console.log('[ChatView] 进入视图' +
                (options && options.fromView ? ' (来自: ' + options.fromView + ')' : ''));

 // 初始化非阻塞进度条（嵌入模型下载 / 索引重建）
            if (typeof RamariaProgressBar !== 'undefined') {
                try {
                    RamariaProgressBar.init();
                    console.log('[ChatView] 进度条组件已初始化');
                } catch (err) {
                    console.warn('[ChatView] 进度条初始化失败:', err);
                }
            }

 // 首次渲染
            render();

 // ★ v1.2 M5-B: 若来自记忆页，显示面包屑导航
            _handleBreadcrumb(options);

 // ★ v1.2 M5-A: 初始化 SessionDrawer 组件
            _initSessionDrawer();

 // ★ v1.2 M5-B: 若有目标 sessionId（来自 L1 卡片跳转），加载该会话
            // 注意：必须 await 完成后再执行 _loadInitialData，
            // 否则 persona selector 的自动匹配可能覆盖跳转加载的会话。
            _handleSessionJump(options).then(function () {
                // 跳转加载完成后，再执行常规的初始数据加载
                _loadInitialData();
            });

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
            console.log('[ChatView] 离开视图');

 // 销毁进度条组件，释放事件监听
            if (typeof RamariaProgressBar !== 'undefined') {
                try {
                    RamariaProgressBar.destroy();
                    console.log('[ChatView] 进度条组件已销毁');
                } catch (err) {
                    console.warn('[ChatView] 进度条销毁失败:', err);
                }
            }

 // ★ v1.2 M5-A: 销毁 SessionDrawer 组件
            _destroySessionDrawer();

 // ★ v1.2 M5-B: 移除面包屑（避免残留到其他视图）
            _removeBreadcrumb();

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

 // =========================================================
 // ★ v1.2 M5-A: SessionDrawer 集成
 // =========================================================

 /**
 * 初始化 SessionDrawer 组件。
 *
 * 说明:
 * - 在 ChatView enter 钩子中调用（每次进入对话视图时重新初始化）。
 * - 注册会话选中回调：点击抽屉中的会话项 → 加载该会话的消息。
 * - 绑定 Header "历史" 按钮 → toggle 抽屉。
 * - 若 SessionDrawer 组件未加载（JS 缺失），静默跳过。
 */
    function _initSessionDrawer() {
        if (typeof RamariaSessionDrawer === 'undefined') {
            console.warn('[ChatView] RamariaSessionDrawer 未加载，跳过初始化');
            return;
        }

        try {
            RamariaSessionDrawer.init({
                onSelect: function (sessionId, session) {
                    _onSessionDrawerSelect(sessionId, session);
                }
            });

            // 绑定 Header "历史" 按钮
            setTimeout(function () {
                var historyBtn = document.getElementById('chat-history-btn');
                if (historyBtn) {
                    historyBtn.addEventListener('click', function () {
                        var personaUid = _sessionPersonaUid ||
                            RamariaStore.get('currentPersonaUid') ||
                            'rama-0001';
                        RamariaSessionDrawer.toggle(personaUid);
                    });
                    console.log('[ChatView] SessionDrawer 历史按钮已绑定');
                }
            }, 200);

            console.log('[ChatView] SessionDrawer 已初始化');
        } catch (err) {
            console.error('[ChatView] SessionDrawer 初始化失败:', err);
        }
    }

 /**
 * 销毁 SessionDrawer 组件。
 * 在 ChatView leave 钩子中调用。
 */
    function _destroySessionDrawer() {
        if (typeof RamariaSessionDrawer === 'undefined') return;
        try {
            RamariaSessionDrawer.destroy();
            console.log('[ChatView] SessionDrawer 已销毁');
        } catch (err) {
            console.warn('[ChatView] SessionDrawer 销毁失败:', err);
        }
    }

 // =========================================================
 // ★ v1.2 M5-B: 面包屑导航 + 会话跳转
 // =========================================================

 /**
 * 处理面包屑导航。
 *
 * 说明:
 * - 当 ChatView 从记忆页跳转而来（options.fromView === 'memory'），
 *   在聊天页眉上方渲染一条面包屑："← 返回记忆"。
 * - 点击面包屑 → 导航回记忆页。
 * - 面包屑在 leave 钩子中移除（通过 _removeBreadcrumb）。
 *
 * 参数:
 * - `options`: Router 传入的导航 options。
 */
    function _handleBreadcrumb(options) {
        if (!options || options.fromView !== 'memory') {
            // 非记忆页跳转，移除可能残留的面包屑
            _removeBreadcrumb();
            return;
        }

        console.log('[ChatView] 来自记忆页，显示面包屑导航');

        // 移除旧面包屑（防止重复）
        _removeBreadcrumb();

        // 创建面包屑
        var container = document.getElementById('view-chat');
        if (!container) return;

        var breadcrumb = document.createElement('div');
        breadcrumb.className = 'chat-breadcrumb';
        breadcrumb.id = 'chat-breadcrumb';
        breadcrumb.innerHTML =
            '<button class="chat-breadcrumb-btn" id="chat-breadcrumb-back" ' +
                'aria-label="返回记忆页" title="返回记忆页">' +
                '← 返回记忆' +
            '</button>';

        // 插入到 chat-header 之前
        var header = document.getElementById('chat-header');
        if (header && header.parentNode === container) {
            container.insertBefore(breadcrumb, header);
        } else {
            container.insertBefore(breadcrumb, container.firstChild);
        }

        // 绑定点击事件
        var backBtn = document.getElementById('chat-breadcrumb-back');
        if (backBtn) {
            backBtn.addEventListener('click', function () {
                console.log('[ChatView] 面包屑 → 返回记忆页');
                RamariaRouter.showView('memory');
            });
        }
    }

 /**
 * 移除面包屑 DOM 元素。
 */
    function _removeBreadcrumb() {
        var el = document.getElementById('chat-breadcrumb');
        if (el && el.parentNode) {
            el.parentNode.removeChild(el);
        }
    }

 /**
 * 处理从 L1 记忆卡片跳转到指定会话。
 *
 * 说明:
 * - 当 options.sessionId 存在时（来自记忆页 L1 卡片"查看对话"按钮），
 *   加载该 session 的完整消息列表。
 * - 此操作在 _loadInitialData 之前执行——若加载成功，
 *   _loadInitialData 中与 persona 自动匹配的逻辑会被跳过（已有消息）。
 * - 若 sessionId 对应的 session 已关闭，自动设置为只读模式。
 *
 * 参数:
 * - `options`: Router 传入的导航 options。
 */
    async function _handleSessionJump(options) {
        if (!options || !options.sessionId) {
            _sessionJumped = false;
            return; // 非跳转场景，正常加载
        }

        var sessionId = options.sessionId;
        var personaUid = options.personaUid || null;

        console.log('[ChatView] L1 记忆卡片跳转: session=' + sessionId.substring(0, 8) +
            ', persona=' + personaUid);

        try {
            // 加载会话详情（含消息）
            var detail = await RamariaApi.session.get(sessionId);

            if (!detail) {
                console.warn('[ChatView] 跳转目标会话不存在: ' + sessionId);
                RamariaToast.show('warning', '会话不存在', '该会话可能已被删除，回到当前对话');
                return;
            }

            var isClosed = !!detail.ended_at;
            var messages = detail.messages || [];
            var dbPersonaUid = detail.persona_uid || null;

            console.log('[ChatView] 跳转加载完成: ' + messages.length + ' 条消息, 已关闭=' + isClosed);

            // 清除空状态
            _clearMessages();

            // 设置消息
            RamariaStore.set('messages', messages);
            RamariaStore.set('activeSessionId', sessionId);

            // 同步 persona_uid
            if (dbPersonaUid) {
                RamariaStore.set('sessionPersonaUid', dbPersonaUid);
                _sessionPersonaUid = dbPersonaUid;

                // 同步下拉框
                var select = $('chat-persona-select');
                if (select && select.value !== dbPersonaUid) {
                    var optionExists = false;
                    for (var i = 0; i < select.options.length; i++) {
                        if (select.options[i].value === dbPersonaUid) {
                            optionExists = true;
                            break;
                        }
                    }
                    if (optionExists) {
                        select.value = dbPersonaUid;
                        RamariaStore.set('currentPersonaUid', dbPersonaUid);
                        _updateHeaderPersona();
                    }
                }
            } else if (personaUid) {
                // session 为存量 NULL 数据，使用前端传入的 personaUid
                RamariaStore.set('sessionPersonaUid', null);
                _sessionPersonaUid = null;
            }

            // 设置只读模式（已关闭 session）
            _setReadonlyMode(isClosed);

            // 更新 persona 会话缓存
            var effectivePersona = dbPersonaUid || personaUid;
            if (effectivePersona) {
                RamariaStore.setPersonaSession(effectivePersona, sessionId);
                _persistPersonaSessions().catch(function (err) {
                    console.warn('[ChatView] 跳转后缓存持久化失败:', err);
                });
            }

            // 全量渲染消息
            _renderAllMessages();

            // ★ 标记已通过跳转加载会话，防止 _loadInitialData 覆盖
            _sessionJumped = true;

            RamariaToast.show('info', '已加载历史对话',
                messages.length + ' 条消息' + (isClosed ? '（只读）' : ''));

        } catch (err) {
            console.error('[ChatView] 跳转加载会话失败:', err);
            _sessionJumped = false;
            RamariaToast.show('error', '加载失败', err.message || '无法加载会话消息');
        }
    }

 // =========================================================

 /**
 * ★ v1.2 M5-A: 当用户在 SessionDrawer 中点击某个会话项时调用。
 *
 * 流程:
 * 1. 从后端加载该 session 的完整详情（含消息列表）。
 * 2. 替换当前 ChatView 的消息列表为该 session 的消息。
 * 3. 根据 session.ended_at 判断只读模式。
 * 4. 同步 persona_uid 归属（前端 Store + 内部状态）。
 * 5. 导入会话：不显示发送框（只读），显示来源标签。
 *
 * 参数:
 * - `sessionId`: 会话 UUID。
 * - `sessionSummary`: 会话摘要（来自 SessionDrawer 的列表项数据）。
 *
 * 容错:
 * - session 不存在 → Toast 提示。
 * - 加载失败 → Toast 提示，不改变当前界面。
 * - 流式进行中 → 拒绝操作（Toast 提示）。
 */
    async function _onSessionDrawerSelect(sessionId, sessionSummary) {
        if (!sessionId) return;

        // 流式进行中时不可切换会话
        if (RamariaStore.get('isStreaming')) {
            RamariaToast.show('warning', '请等待当前回复完成后再切换会话');
            return;
        }

        console.log('[ChatView] SessionDrawer 选中会话: ' + sessionId.substring(0, 8) +
            ' (ended_at=' + (sessionSummary ? sessionSummary.ended_at : '?') + ')');

        try {
            // 加载会话详情（含消息）
            var detail = await RamariaApi.session.get(sessionId);

            if (!detail) {
                RamariaToast.show('error', '会话不存在', '该会话可能已被删除');
                return;
            }

            var isClosed = !!detail.ended_at;
            var messages = detail.messages || [];
            var dbPersonaUid = detail.persona_uid || null;

            console.log('[ChatView] 加载会话 ' + sessionId.substring(0, 8) +
                ': ' + messages.length + ' 条消息, 已关闭=' + isClosed +
                ', persona_uid=' + (dbPersonaUid || '(null)'));

            // 清除当前消息
            _clearMessages();

            // 设置消息到 Store
            RamariaStore.set('messages', messages);
            RamariaStore.set('activeSessionId', sessionId);

            // 同步 persona_uid
            if (dbPersonaUid) {
                RamariaStore.set('sessionPersonaUid', dbPersonaUid);
                _sessionPersonaUid = dbPersonaUid;

                // 同步下拉框选择
                var select = $('chat-persona-select');
                if (select) {
                    // 检查该 persona 是否在下拉框中
                    var optionExists = false;
                    for (var i = 0; i < select.options.length; i++) {
                        if (select.options[i].value === dbPersonaUid) {
                            optionExists = true;
                            break;
                        }
                    }
                    if (optionExists && select.value !== dbPersonaUid) {
                        select.value = dbPersonaUid;
                        RamariaStore.set('currentPersonaUid', dbPersonaUid);
                        _updateHeaderPersona();
                    }
                }
            } else {
                // 存量 session: persona_uid 为 NULL
                RamariaStore.set('sessionPersonaUid', null);
                _sessionPersonaUid = null;
                // 保留 currentPersonaUid（前端选择的人格）
            }

            // 设置只读模式
            _setReadonlyMode(isClosed);

            // 更新 persona 会话缓存
            if (dbPersonaUid) {
                RamariaStore.setPersonaSession(dbPersonaUid, sessionId);
                _persistPersonaSessions().catch(function (err) {
                    console.warn('[ChatView] 缓存持久化失败:', err);
                });
            }

            // 全量渲染消息
            _renderAllMessages();

            RamariaToast.show('info', '已加载会话',
                messages.length + ' 条消息' + (isClosed ? '（只读）' : ''));

        } catch (err) {
            console.error('[ChatView] SessionDrawer 加载会话失败:', err);
            RamariaToast.show('error', '加载失败', err.message || '无法加载会话消息');
        }
    }

 // =========================================================

    async function _loadInitialData() {
        try {
 // ★ v1.2 M5-B: 若已通过 L1 卡片跳转加载了会话，跳过自动匹配和消息加载
 // 仅刷新 persona 选择器（下拉框可能不含跳转 persona），保留已加载的会话数据。
            if (_sessionJumped) {
                console.log('[ChatView] 已通过跳转加载会话，跳过自动 persona 匹配');
                // 刷新下拉框以包含跳转的 persona（但不改变选中值）
                await _refreshPersonaSelector(true); // silent: 不触发自动切换
                // 重置标记（仅对本次 enter 生效）
                _sessionJumped = false;
                return;
            }

 // ── 先恢复人格会话映射（从后端 settings 读取）──
            await _restorePersonaSessions();

 // 加载人格列表（联动页眉名称）
            await _refreshPersonaSelector();

 // 加载会话列表
            var sessions = [];
            try {
                sessions = await RamariaApi.session.list();
                RamariaStore.set('sessions', sessions || []);
            } catch (err) {
                console.warn('[ChatView] 加载会话列表失败:', err);
            }

 // ── 检查是否从导入完成页导航过来 ──
            var viewingImported = RamariaStore.get('viewingImportedSession');
            if (viewingImported) {
                var importedName = RamariaStore.get('viewingImportedName') || '';
                if (importedName) {
                    var nameEl = document.getElementById('chat-header-persona-name');
                    if (nameEl) nameEl.textContent = '📥 ' + _escHtml(importedName) + ' 的导入消息';
                    var statusEl = document.getElementById('chat-header-status');
                    if (statusEl) {
                        statusEl.classList.remove('active');
                        statusEl.classList.add('inactive');
                        statusEl.title = '导入历史';
                    }
                }

                if (sessions && sessions.length > 0) {
                    var latest = sessions[sessions.length - 1];
                    try {
                        var histSession = await RamariaApi.session.get(latest.id);
                        RamariaStore.set('messages', histSession.messages || []);
                        _renderAllMessages();
                        RamariaStore.set('activeSessionId', null);
                        RamariaRouter.setSessionInfo('📥 导入: ' + latest.id.substring(0, 8) + '...');
                    } catch (err) {
                        console.warn('[ChatView] 加载导入会话失败:', err);
                    }
                }

                RamariaStore.set('viewingImportedSession', false);
                RamariaStore.set('viewingImportedName', '');
                return;
            }

 // ── 恢复上次对话 ──
 // 获取当前选中的 persona（默认 rama-0001）
            var personaSelect = $('chat-persona-select');
            var currentPersona = personaSelect ? personaSelect.value : 'rama-0001';

 // 尝试从持久化映射中恢复该人格的 session
            var savedSessionId = RamariaStore.getPersonaSession(currentPersona);
            var loaded = false;

            if (savedSessionId) {
                try {
                    var session = await RamariaApi.session.get(savedSessionId);
// ★ 防御: 跳过已关闭的 session（ended_at 非空）。
// 已关闭 session 的 personaSessions 映射本应在 _handleSaveSession 中被
// _clearPersonaSessionCache 清除，但若持久化失败或应用未正常退出，
// 后端可能残留过期映射。此处作为最后一道防线确保不会恢复已关闭会话。
                    if (session && session.ended_at != null && session.ended_at !== 0) {
                        console.warn('[ChatView] 跳过已关闭 session: ' + savedSessionId
                            + ' (ended_at=' + session.ended_at + '), 清除过期映射');
                        _clearPersonaSessionCache(currentPersona);
// 持久化清除后的映射，避免每次重启都重复"恢复→跳过→空状态"循环
                        _persistPersonaSessions().catch(function(err) {
                            console.warn('[ChatView] 清除过期映射持久化失败:', err);
                        });
                    } else if (session && session.messages && session.messages.length > 0) {
                        RamariaStore.set('activeSessionId', savedSessionId);
                        RamariaStore.set('messages', session.messages);
                        RamariaStore.set('currentPersonaUid', currentPersona);
                        _renderAllMessages();
                        loaded = true;

 // ★ v1.2: 从后端 session.persona_uid 读取真相源并同步到 Store
                        var dbPersona = session.persona_uid || null;
                        if (dbPersona) {
                            _sessionPersonaUid = dbPersona;
                            RamariaStore.set('sessionPersonaUid', dbPersona);
                            console.log('[ChatView] 恢复会话: ' + savedSessionId
                                + ' (' + session.messages.length + ' 条消息, persona_uid=' + dbPersona + ')');
                        } else {
 // 存量兼容：session 无 persona_uid 时使用缓存 persona 作为默认值
                            _sessionPersonaUid = currentPersona;
                            RamariaStore.set('sessionPersonaUid', currentPersona);
                            console.log('[ChatView] 恢复会话: ' + savedSessionId
                                + ' (' + session.messages.length + ' 条消息, persona_uid=NULL, 回退=' + currentPersona + ')');
                        }

 // ★ 不启用只读模式：用户回到对话页就是要继续聊的。
                        RamariaRouter.setSessionInfo('会话: ' + savedSessionId.substring(0, 8) + '...');
                    }
                } catch (err) {
                    console.warn('[ChatView] 恢复持久化会话失败:', err);
                }
            }

            if (!loaded) {
 // 无持久化映射或 session 已失效，显示空状态
                RamariaStore.set('activeSessionId', null);
                RamariaStore.set('messages', []);
                RamariaStore.set('currentPersonaUid', currentPersona);
                RamariaStore.set('sessionPersonaUid', null);
                _sessionPersonaUid = null;
                _renderAllMessages();
                _setReadonlyMode(false);
                console.log('[ChatView] 无历史会话，显示空状态');
            }

 // 同步页眉
            _updateHeaderPersona();
        } catch (err) {
            console.error('[ChatView] 加载初始数据失败:', err);
        }
    }

 /** HTML 实体转义（内联辅助函数，避免跨文件依赖） */
    function _escHtml(text) {
        if (!text) return '';
        return String(text)
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;');
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
