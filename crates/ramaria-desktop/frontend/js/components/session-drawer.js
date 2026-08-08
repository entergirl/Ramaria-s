/**
 * js/components/session-drawer.js — Ramaria SessionDrawer 会话历史抽屉
 *
 * 职责:
 * - 左侧滑出抽屉面板，列出当前 persona 的所有会话（活跃/已关闭/导入）
 * - 支持搜索过滤（按 persona 名称或会话时间）
 * - 点击会话项 → 通过回调通知 ChatView 加载该会话的消息
 * - 三种会话状态区分：活跃（绿色圆点）、已关闭（灰色时间戳）、导入（来源标签）
 *
 * 设计特点:
 * - 独立组件，通过回调（onSelect）与 ChatView 解耦
 * - CSS 动画由 session-drawer.css 驱动（180ms slide）
 * - 会话列表按 started_at 倒序排列
 * - 响应式：窗口宽度 ≤640px 时全宽覆盖
 * - 空状态显示引导提示
 * - 加载失败显示重试按钮
 *
 * 用法:
 *   RamariaSessionDrawer.init({
 *     onSelect: function(sessionId, session) { ... }
 *   });
 *   RamariaSessionDrawer.show(personaUid);
 *   RamariaSessionDrawer.hide();
 *
 * 依赖:
 * - RamariaApi（js/api.js）
 * - RamariaStore（js/store.js）
 * - RamariaFormat（js/utils/format.js）
 * - CSS: css/components/session-drawer.css
 */

var RamariaSessionDrawer = (function () {
    'use strict';

    // =========================================================
    // 内部状态
    // =========================================================

    /** 抽屉是否打开 */
    var _isOpen = false;

    /** 是否正在异步加载会话列表（防并发 show() 调用） */
    var _loading = false;

    /** 当前加载的会话列表 */
    var _sessions = [];

    /** 当前筛选的人格 UID */
    var _currentPersonaUid = null;

    /** 搜索过滤文本 */
    var _filterText = '';

    /** 会话选中回调: function(sessionId, sessionSummary) */
    var _onSelect = null;

    /** DOM 引用缓存 */
    var _dom = {};

    /** 是否已初始化 */
    var _initialized = false;

    // =========================================================
    // DOM 创建
    // =========================================================

    /**
     * 创建抽屉 DOM 结构并注入到 #view-chat 容器。
     *
     * 结构:
     *   #session-drawer
     *     .session-drawer-header
     *       .session-drawer-title
     *       .session-drawer-close-btn
     *     .session-drawer-search
     *       input.session-drawer-search-input
     *     .session-drawer-list
     *     .session-drawer-footer
     *
     * 抽屉定位为 #view-chat 内的 position:absolute 覆盖层,
     * 初始 translateX(-100%) 隐藏在左侧视口外。
     */
    function _createDom() {
        var container = document.getElementById('view-chat');
        if (!container) {
            console.error('[SessionDrawer] 找不到 #view-chat 容器');
            return false;
        }

        // 避免重复创建
        var existing = document.getElementById('session-drawer');
        if (existing) {
            existing.remove();
        }

        var drawer = document.createElement('aside');
        drawer.id = 'session-drawer';
        drawer.className = 'session-drawer';
        drawer.setAttribute('aria-label', '会话历史');
        drawer.setAttribute('aria-hidden', 'true');
        drawer.setAttribute('role', 'complementary');

        drawer.innerHTML =
            // ── 头部 ──
            '<div class="session-drawer-header">' +
                '<h3 class="session-drawer-title">会话历史</h3>' +
                '<button class="session-drawer-close-btn" id="session-drawer-close" ' +
                    'aria-label="关闭历史面板" title="关闭 (Esc)">✕</button>' +
            '</div>' +
            // ── 搜索栏 ──
            '<div class="session-drawer-search">' +
                '<input type="text" class="session-drawer-search-input" id="session-drawer-search-input" ' +
                    'placeholder="🔍 搜索会话..." aria-label="搜索会话">' +
            '</div>' +
            // ── 会话列表 ──
            '<div class="session-drawer-list" id="session-drawer-list" role="list"></div>' +
            // ── 底部 ──
            '<div class="session-drawer-footer">' +
                '<button class="btn btn-ghost btn-sm session-drawer-current-btn" id="session-drawer-current-btn">' +
                    '返回当前对话' +
                '</button>' +
            '</div>';

        container.appendChild(drawer);

        // 缓存 DOM 引用
        _dom.drawer = drawer;
        _dom.list = document.getElementById('session-drawer-list');
        _dom.searchInput = document.getElementById('session-drawer-search-input');
        _dom.closeBtn = document.getElementById('session-drawer-close');
        _dom.currentBtn = document.getElementById('session-drawer-current-btn');

        return true;
    }

    /**
     * 绑定抽屉内部事件。
     * 关闭按钮 / "返回当前对话"按钮 / 搜索过滤 / ESC 键 / 点击遮罩区域。
     */
    function _bindEvents() {
        if (!_dom.drawer) return;

        // 关闭按钮
        if (_dom.closeBtn) {
            _dom.closeBtn.addEventListener('click', function () {
                hide();
            });
        }

        // "返回当前对话" 按钮
        if (_dom.currentBtn) {
            _dom.currentBtn.addEventListener('click', function () {
                hide();
            });
        }

        // 搜索过滤
        if (_dom.searchInput) {
            _dom.searchInput.addEventListener('input', function () {
                _filterText = this.value.trim().toLowerCase();
                _renderSessionList();
            });
        }

        // ESC 键关闭
        _dom.drawer.addEventListener('keydown', function (e) {
            if (e.key === 'Escape') {
                hide();
            }
        });

        // 点击抽屉外区域（#view-chat 内非抽屉部分）关闭
        // 由于抽屉是绝对定位覆盖在聊天内容上，点击抽屉本身不触发关闭
        // 这里委托到 #view-chat 容器上的点击事件
        var viewChat = document.getElementById('view-chat');
        if (viewChat) {
            viewChat.addEventListener('click', function (e) {
                if (!_isOpen) return;
                // 如果点击目标不在抽屉内部，关闭抽屉
                if (_dom.drawer && !_dom.drawer.contains(e.target)) {
                    hide();
                }
            });
        }
    }

    // =========================================================
    // 会话加载
    // =========================================================

    /**
     * 从后端加载所有会话，按当前 persona_uid 过滤。
     *
     * 说明:
     * - 调用 RamariaApi.session.listSessions() 获取全部会话。
     * - 前端按 persona_uid 过滤（因后端 list_sessions 暂不支持按 persona 筛选参数）。
     * - 按 started_at 倒序排列。
     * - 加载失败时显示错误提示和重试按钮。
     *
     * 返回:
     * - 加载成功返回 true，失败返回 false。
     */
    async function _loadSessions() {
        try {
            var allSessions = await RamariaApi.session.list();
            if (!Array.isArray(allSessions)) {
                console.warn('[SessionDrawer] listSessions 返回非数组:', typeof allSessions);
                allSessions = [];
            }

            // 按当前 persona_uid 过滤
            if (_currentPersonaUid) {
                _sessions = allSessions.filter(function (s) {
                    // 匹配 persona_uid（NULL 的存量 session 归入默认人格）
                    if (!s.persona_uid) {
                        // 存量 NULL session：仅当当前人格是默认人格(rama-0001)时显示。
                        // P0-1 修复后新建会话都会绑定 persona_uid，此处仅为
                        // 旧数据兼容；命中时告警便于发现归属缺失的存量会话。
                        if (_currentPersonaUid === 'rama-0001') {
                            console.warn(
                                '[SessionDrawer] 会话归属缺失（persona_uid=NULL）：' +
                                (s.id || '') + '，按存量兼容归入默认人格 rama-0001'
                            );
                            return true;
                        }
                        return false;
                    }
                    return s.persona_uid === _currentPersonaUid;
                });
            } else {
                // 无指定人格时显示全部
                _sessions = allSessions;
            }

            // 按 started_at 倒序（后端已排序，此处防御性重排）
            _sessions.sort(function (a, b) {
                return (b.started_at || 0) - (a.started_at || 0);
            });

            console.log('[SessionDrawer] 加载 ' + _sessions.length + ' 个会话 (persona=' +
                (_currentPersonaUid || 'all') + ', 总数=' + allSessions.length + ')');
            return true;
        } catch (err) {
            console.error('[SessionDrawer] 加载会话列表失败:', err);
            _sessions = [];
            return false;
        }
    }

    // =========================================================
    // 渲染
    // =========================================================

    /**
     * 渲染会话列表。
     *
     * 过滤逻辑:
     * - 先按 _filterText 过滤（匹配 persona_name / session id / 时间）。
     * - 按状态分组：活跃（ended_at === null）在前，已关闭在后。
     * - 导入会话通过 persona_uid 前缀（char-/anim-/oc-/hist-）推断。
     */
    function _renderSessionList() {
        var listEl = _dom.list;
        if (!listEl) return;

        listEl.innerHTML = '';

        // 空状态
        if (!_sessions || _sessions.length === 0) {
            listEl.innerHTML =
                '<div class="session-drawer-empty">' +
                    '<div class="session-drawer-empty-icon">📋</div>' +
                    '<div class="session-drawer-empty-text">' +
                        '暂无会话历史<br>' +
                        '<small>开始对话后自动记录</small>' +
                    '</div>' +
                '</div>';
            return;
        }

        // 过滤
        var filtered = _sessions;
        if (_filterText) {
            filtered = _sessions.filter(function (s) {
                // 按 persona_uid 匹配
                if (s.persona_uid && s.persona_uid.toLowerCase().indexOf(_filterText) !== -1) return true;
                // 按 session id 短格式匹配
                if (s.id && s.id.substring(0, 8).toLowerCase().indexOf(_filterText) !== -1) return true;
                // 按时间文本匹配
                var timeStr = RamariaFormat ? RamariaFormat.smartTime(s.started_at) : '';
                if (timeStr && timeStr.toLowerCase().indexOf(_filterText) !== -1) return true;
                return false;
            });
        }

        if (filtered.length === 0) {
            listEl.innerHTML =
                '<div class="session-drawer-empty">' +
                    '<div class="session-drawer-empty-text">无匹配会话</div>' +
                '</div>';
            return;
        }

        // 分组：活跃在前，已关闭在后
        var activeSessions = [];
        var closedSessions = [];
        for (var i = 0; i < filtered.length; i++) {
            var s = filtered[i];
            if (s.ended_at == null) {
                activeSessions.push(s);
            } else {
                closedSessions.push(s);
            }
        }

        // 渲染活跃会话组
        if (activeSessions.length > 0) {
            var activeGroup = _createGroupHeader('活跃', activeSessions.length);
            listEl.appendChild(activeGroup);
            for (var j = 0; j < activeSessions.length; j++) {
                listEl.appendChild(_createSessionItem(activeSessions[j], true));
            }
        }

        // 渲染已关闭会话组
        if (closedSessions.length > 0) {
            var closedGroup = _createGroupHeader('已关闭', closedSessions.length);
            listEl.appendChild(closedGroup);
            for (var k = 0; k < closedSessions.length; k++) {
                listEl.appendChild(_createSessionItem(closedSessions[k], false));
            }
        }
    }

    /**
     * 创建分组标题元素。
     *
     * 参数:
     * - `label`: 分组名称（"活跃" | "已关闭"）
     * - `count`: 该分组中会话数量
     */
    function _createGroupHeader(label, count) {
        var el = document.createElement('div');
        el.className = 'session-drawer-group-header';
        el.innerHTML = '<span>' + label + '</span>' +
            '<span class="session-drawer-group-count">' + count + '</span>';
        return el;
    }

    /**
     * 创建单个会话项 DOM 元素。
     *
     * 参数:
     * - `session`: 会话摘要对象 { id, started_at, ended_at, message_count, persona_uid }
     * - `isActive`: 是否为活跃会话（ended_at === null）
     *
     * 布局:
     *   .session-drawer-item
     *     .session-drawer-item-status (绿色圆点 或 灰色时间)
     *     .session-drawer-item-info
     *       .session-drawer-item-title (时间 + 状态标签)
     *       .session-drawer-item-meta (消息数 + persona名称)
     */
    function _createSessionItem(session, isActive) {
        var item = document.createElement('div');
        item.className = 'session-drawer-item';
        item.setAttribute('role', 'listitem');
        item.setAttribute('tabindex', '0');
        item.setAttribute('data-session-id', session.id || '');

        // 状态指示器
        var statusClass = isActive ? 'session-drawer-item-status session-drawer-item-status--active' :
                                     'session-drawer-item-status';
        var statusHtml;
        if (isActive) {
            statusHtml = '<span class="' + statusClass + '" title="对话中">' +
                '<span class="session-drawer-status-dot"></span>' +
                '</span>';
        } else {
            // 已关闭：显示简短时间
            var closedTime = RamariaFormat ? RamariaFormat.smartTime(session.ended_at || session.started_at) : '';
            statusHtml = '<span class="' + statusClass + '">' + _escapeHtml(closedTime) + '</span>';
        }

        // 标题行：时间 + 状态标签
        var timeLabel = RamariaFormat ? RamariaFormat.smartTime(session.started_at) :
                        (session.started_at ? new Date(session.started_at).toLocaleDateString() : '未知时间');
        var tagHtml = '';
        if (isActive) {
            tagHtml = '<span class="session-drawer-item-tag session-drawer-item-tag--active">活跃</span>';
        } else {
            // 判断是否为导入会话（persona_uid 以 char-/anim-/oc-/hist- 开头）
            if (_isImportedSession(session)) {
                var personaName = _getPersonaName(session.persona_uid);
                tagHtml = '<span class="session-drawer-item-tag session-drawer-item-tag--import">导入: ' +
                    _escapeHtml(personaName) + '</span>';
            }
        }

        // 消息数
        var msgCount = session.message_count || 0;
        var msgLabel = msgCount > 0 ? (msgCount + ' 条消息') : '无消息';

        // persona 名称
        var personaLabel = _getPersonaName(session.persona_uid);

        item.innerHTML =
            '<div class="session-drawer-item-inner">' +
                '<div class="session-drawer-item-status-col">' + statusHtml + '</div>' +
                '<div class="session-drawer-item-info">' +
                    '<div class="session-drawer-item-title">' +
                        '<span class="session-drawer-item-time">' + _escapeHtml(timeLabel) + '</span>' +
                        tagHtml +
                    '</div>' +
                    '<div class="session-drawer-item-meta">' +
                        '<span>' + msgLabel + '</span>' +
                        (personaLabel ? '<span class="session-drawer-item-persona"> — ' +
                            _escapeHtml(personaLabel) + '</span>' : '') +
                    '</div>' +
                '</div>' +
            '</div>';

        // 点击事件
        item.addEventListener('click', function () {
            _handleSessionClick(session);
        });

        // 键盘可访问性（Enter/Space 触发点击）
        item.addEventListener('keydown', function (e) {
            if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                _handleSessionClick(session);
            }
        });

        return item;
    }

    /**
     * 判断会话是否为导入会话。
     * 导入 persona 的 uid 以特定前缀开头。
     */
    function _isImportedSession(session) {
        if (!session.persona_uid) return false;
        var uid = session.persona_uid;
        return uid.indexOf('char-') === 0 ||
               uid.indexOf('anim-') === 0 ||
               uid.indexOf('oc-') === 0 ||
               uid.indexOf('hist-') === 0;
    }

    /**
     * 根据 persona_uid 获取 persona 显示名称。
     * 从 Store 中缓存的 personas 列表查找。
     */
    function _getPersonaName(personaUid) {
        if (!personaUid) return '';
        // 默认人格使用中文名
        if (personaUid === 'rama-0001') return 'Rama';

        var personas = RamariaStore.get('personas') || [];
        for (var i = 0; i < personas.length; i++) {
            if (personas[i].uid === personaUid) {
                return personas[i].name || personaUid;
            }
        }
        // 回退：显示 uid 的短格式
        return personaUid.substring(0, 8) + '…';
    }

    /**
     * 处理会话点击事件。
     *
     * 调用 _onSelect 回调通知 ChatView 加载该会话。
     * 点击后关闭抽屉。
     */
    function _handleSessionClick(session) {
        if (!session || !session.id) return;

        console.log('[SessionDrawer] 选中会话: ' + session.id.substring(0, 8) +
            ' (消息数=' + (session.message_count || 0) +
            ', 活跃=' + (session.ended_at == null) + ')');

        if (typeof _onSelect === 'function') {
            _onSelect(session.id, session);
        }

        // 选中后关闭抽屉
        hide();
    }

    /**
     * HTML 转义（防 XSS）。
     */
    function _escapeHtml(text) {
        if (!text) return '';
        var div = document.createElement('div');
        div.appendChild(document.createTextNode(text));
        return div.innerHTML;
    }

    // =========================================================
    // 加载/错误状态渲染
    // =========================================================

    /**
     * 显示加载中骨架屏。
     */
    function _showLoading() {
        if (!_dom.list) return;
        _dom.list.innerHTML =
            '<div class="session-drawer-loading">' +
                '<div class="session-drawer-skeleton"></div>' +
                '<div class="session-drawer-skeleton"></div>' +
                '<div class="session-drawer-skeleton"></div>' +
            '</div>';
    }

    /**
     * 显示加载错误和重试按钮。
     */
    function _showError(errMsg) {
        if (!_dom.list) return;
        _dom.list.innerHTML =
            '<div class="session-drawer-error">' +
                '<div class="session-drawer-error-icon">⚠️</div>' +
                '<div class="session-drawer-error-text">加载失败</div>' +
                '<div class="session-drawer-error-detail">' + _escapeHtml(errMsg || '未知错误') + '</div>' +
                '<button class="btn btn-secondary btn-sm" id="session-drawer-retry-btn">🔄 重试</button>' +
            '</div>';

        // 绑定重试按钮
        var retryBtn = document.getElementById('session-drawer-retry-btn');
        if (retryBtn) {
            retryBtn.addEventListener('click', function () {
                _showLoading();
                _loadSessions().then(function (ok) {
                    if (ok) {
                        _renderSessionList();
                    } else {
                        _showError('重试失败');
                    }
                });
            });
        }
    }

    // =========================================================
    // 公开 API
    // =========================================================

    /**
     * 初始化 SessionDrawer 组件。
     *
     * 参数:
     * - `options.onSelect`: function(sessionId, sessionSummary) — 选中会话时的回调
     *
     * 说明:
     * - 创建 DOM 并绑定事件，但不立即显示。
     * - 重复调用会先销毁旧实例再初始化。
     */
    function init(options) {
        if (_initialized) {
            console.warn('[SessionDrawer] 已初始化，先销毁旧实例');
            destroy();
        }

        options = options || {};
        _onSelect = options.onSelect || null;

        var ok = _createDom();
        if (!ok) {
            console.error('[SessionDrawer] 初始化失败：无法创建 DOM');
            return;
        }

        _bindEvents();
        _initialized = true;

        console.log('[SessionDrawer] 初始化完成 (onSelect=' + (typeof _onSelect === 'function' ? '已绑定' : '未绑定') + ')');
    }

    /**
     * 显示抽屉面板。
     *
     * 参数:
     * - `personaUid`: 要筛选的人格 UID（可选，默认当前 Store 中的 currentPersonaUid）
     *
     * 流程:
     * 1. 确定筛选的人格 UID。
     * 2. 显示加载中状态。
     * 3. 从后端加载会话列表。
     * 4. 渲染会话列表。
     * 5. 播放滑入动画（CSS class 切换）。
     *
     * 容错:
     * - 加载失败时显示错误提示 + 重试按钮，不阻塞抽屉显示。
     */
    async function show(personaUid) {
        if (!_initialized) {
            console.warn('[SessionDrawer] 尚未初始化，忽略 show()');
            return;
        }

        // 防止并发 show() 调用（快速双击历史按钮等场景）
        if (_loading) {
            console.log('[SessionDrawer] 正在加载中，忽略重复 show()');
            return;
        }

        if (_isOpen) {
            // 已在打开状态：刷新列表
            console.log('[SessionDrawer] 已在打开状态，刷新列表');
        }

        // 确定人格 UID
        if (personaUid === undefined || personaUid === null) {
            personaUid = RamariaStore.get('currentPersonaUid') || 'rama-0001';
        }
        _currentPersonaUid = personaUid;

        // 标记加载中（防并发 show()）
        _loading = true;

        // 显示加载中
        _showLoading();

        // 打开面板视觉（CSS 动画立即播放，加载骨架屏可见）
        _dom.drawer.classList.add('session-drawer--open');
        _dom.drawer.setAttribute('aria-hidden', 'false');
        // ★ 修复: _isOpen 在异步加载完成后才设为 true，
        // 避免同一次点击事件冒泡到 #view-chat 的 outside-click 处理器，
        // 导致抽屉刚打开就被立即关闭的竞态条件。

        try {
            // 加载会话
            var ok = await _loadSessions();

            // 异步加载完成，标记逻辑状态为已打开
            _isOpen = true;

            if (ok) {
                _renderSessionList();
                // 重置搜索框
                if (_dom.searchInput) {
                    _dom.searchInput.value = '';
                }
                _filterText = '';
            } else {
                _showError('无法加载会话列表，请检查后端连接');
            }

            // 搜索框聚焦
            if (_dom.searchInput) {
                setTimeout(function () {
                    try { _dom.searchInput.focus(); } catch (_) { /* ignore */ }
                }, 200);
            }
        } finally {
            _loading = false;
        }
    }

    /**
     * 关闭抽屉面板（播放滑出动画）。
     */
    function hide() {
        if (!_initialized) return;
        if (!_isOpen && !_loading) return;

        _dom.drawer.classList.remove('session-drawer--open');
        _dom.drawer.setAttribute('aria-hidden', 'true');
        _isOpen = false;
        _loading = false;

        // 清空搜索
        _filterText = '';
        if (_dom.searchInput) {
            _dom.searchInput.value = '';
        }

        console.log('[SessionDrawer] 已关闭');
    }

    /**
     * 切换抽屉显示/隐藏。
     *
     * 参数:
     * - `personaUid`: 可选，显示时使用的人格 UID。
     */
    async function toggle(personaUid) {
        if (_isOpen) {
            hide();
        } else {
            await show(personaUid);
        }
    }

    /**
     * 销毁组件（移除 DOM，解绑事件，清理状态）。
     */
    function destroy() {
        if (_dom.drawer && _dom.drawer.parentNode) {
            _dom.drawer.parentNode.removeChild(_dom.drawer);
        }

        _dom = {};
        _sessions = [];
        _currentPersonaUid = null;
        _filterText = '';
        _isOpen = false;
        _loading = false;
        _onSelect = null;
        _initialized = false;

        console.log('[SessionDrawer] 已销毁');
    }

    // =========================================================
    // 公开 API
    // =========================================================

    return {
        init: init,
        show: show,
        hide: hide,
        toggle: toggle,
        destroy: destroy,

        /** 是否已初始化 */
        isInitialized: function () { return _initialized; },

        /** 是否打开中 */
        isOpen: function () { return _isOpen; },

        /** 获取当前会话列表（只读快照） */
        getSessions: function () { return _sessions.slice(); },
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaSessionDrawer', {
    value: RamariaSessionDrawer,
    writable: false,
    configurable: false,
});
