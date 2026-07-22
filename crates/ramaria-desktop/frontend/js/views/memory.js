/**
 * js/views/memory.js — Ramaria 记忆查看视图
 *
 * 职责:
 * - 三 Tab 记忆查看：L1 会话摘要（Bento Grid）/ L2 事件列表 / L3 性格画像
 * - 按人格筛选记忆（PersonaSelector）
 * - 每层独立数据加载和缓存，切换 Tab 不重新请求
 * - L3: 三层分层展示（base/primary/accent 卡片布局）
 *
 * 设计特点:
 * - 注册 Router enter/leave 钩子
 * - enter 时自动加载当前人格的 L1/L2/L3 + 画像状态数据
 * - 三面板通过 .active 类切换显示，DOM 始终保留（避免重复渲染）
 * - L1 Bento Grid：粉色渐变卡片，显示 salience 强度条 + valence 色条
 * - L2 事件列表：蓝色主题，点击展开详情（源/态度/置信度等）
 * - L3 性格画像：
 *   - 顶部状态指示器（数据不足/初步/可信 + n_total_eff 数值）
 *   - 按 base/primary/accent 三层卡片布局（不同左边框色区分）
 *   - 每条 trait 显示置信度色条（绿≥80%/黄60-80%/橙<60%）
 *   - accent 层显示触发/抑制条件
 *   - "展开证据"按钮 → RamariaTraitEvidence 组件加载完整证据链
 * - 空数据友好提示
 *
 * 依赖:
 * - RamariaApi / RamariaStore / RamariaRouter
 * - RamariaToast / RamariaSkeleton / RamariaFormat
 * - RamariaTraitEvidence（证据链组件）
 * - CSS: css/views/memory.css + css/components/trait-evidence.css
 */

var RamariaMemoryView = (function () {
    'use strict';

 // =========================================================
 // 内部状态
 // =========================================================

    var _unregisterFns = [];
    var _unsubs = [];

 /** 当前选中的人格 UID */
    var _currentPersonaUid = 'rama-0001';

 /** 缓存已加载数据（key: persona_uid） */
    var _cache = {};

 /** 当前激活的 Tab: 'l1' | 'l2' | 'l3' */
    var _activeTab = 'l1';

    /**
     * 标记是否从对话页返回（用于恢复状态）。
     * 在 L1 卡片"查看对话"按钮点击时设为 true，
     * 在 enter 钩子中检测并恢复之前的 persona 和 tab 选择。
     */
    var _returningFromChat = false;

    /**
     * 离开记忆页时的状态快照，用于从对话页返回时恢复。
     * 包含 { personaUid, activeTab }——不持久化到 Store，
     * 仅存活于内存中（页面刷新后会丢失，用户需重新选择）。
     */
    var _savedState = null;

 // =========================================================
 // DOM 快捷查询
 // =========================================================

    function $(id) { return document.getElementById(id); }

 // =========================================================
 // 渲染
 // =========================================================

    function render() {
        var viewEl = $('view-memory');
        if (!viewEl) {
            console.error('[MemoryView] 找不到 #view-memory 容器');
            return;
        }

 // ── render 会重建 DOM，旧的按钮监听器随之销毁 ──
 // 重置标志确保 _updatePipelineButton 在新 DOM 上重新绑定 click 事件。
        _pipelineBtnBound = false;
 // 同时重置运行态，避免上次未完成的任务阻塞新按钮操作。
        _pipelineRunning = false;

        viewEl.innerHTML = '';

        var inner = document.createElement('div');
        inner.className = 'memory-view-inner';
        viewEl.appendChild(inner);

 // ── 工具栏 ──
        var toolbar = document.createElement('div');
        toolbar.className = 'memory-toolbar';
        toolbar.innerHTML =
            '<select id="memory-persona-select" aria-label="选择人格">' +
                '<option value="rama-0001">默认 (rama-0001)</option>' +
            '</select>' +
            '<span class="memory-toolbar-hint">筛选记忆归属人格</span>' +
 // 深度处理按钮（仅导入人格时显示，默认隐藏）
            '<button class="btn btn-secondary btn-sm hidden ml-auto" id="btn-trigger-pipeline" ' +
            'title="对此导入人格的消息执行 L2 事件提取和 L3 性格画像生成">' +
            '🔬 深度处理导入的消息</button>';
        inner.appendChild(toolbar);

 // ── Tab 切换（ARIA Tabs 模式）──
        var tabs = document.createElement('div');
        tabs.className = 'memory-tabs';
        tabs.setAttribute('role', 'tablist');
        tabs.setAttribute('aria-label', '记忆层级切换');
        tabs.innerHTML =
            '<button class="memory-tab active" data-tab="l1" role="tab" aria-selected="true" aria-controls="memory-panel-l1">' +
                '📄 L1 摘要' +
                '<span class="memory-tab-badge" id="memory-badge-l1">-</span>' +
            '</button>' +
            '<button class="memory-tab" data-tab="l2" role="tab" aria-selected="false" aria-controls="memory-panel-l2">' +
                '📋 L2 事件' +
                '<span class="memory-tab-badge" id="memory-badge-l2">-</span>' +
            '</button>' +
            '<button class="memory-tab" data-tab="l3" role="tab" aria-selected="false" aria-controls="memory-panel-l3">' +
                '🏷️ L3 性格' +
                '<span class="memory-tab-badge" id="memory-badge-l3">-</span>' +
            '</button>';
        inner.appendChild(tabs);

 // ── 面板（ARIA tabpanel）──
        var panelL1 = document.createElement('div');
        panelL1.className = 'memory-panel active';
        panelL1.id = 'memory-panel-l1';
        panelL1.setAttribute('role', 'tabpanel');
        panelL1.setAttribute('aria-labelledby', '');
        panelL1.setAttribute('data-panel', 'l1');
        inner.appendChild(panelL1);

        var panelL2 = document.createElement('div');
        panelL2.className = 'memory-panel';
        panelL2.id = 'memory-panel-l2';
        panelL2.setAttribute('role', 'tabpanel');
        panelL2.setAttribute('aria-labelledby', '');
        panelL2.setAttribute('data-panel', 'l2');
        inner.appendChild(panelL2);

        var panelL3 = document.createElement('div');
        panelL3.className = 'memory-panel';
        panelL3.id = 'memory-panel-l3';
        panelL3.setAttribute('role', 'tabpanel');
        panelL3.setAttribute('aria-labelledby', '');
        panelL3.setAttribute('data-panel', 'l3');
        inner.appendChild(panelL3);

 // ── 事件绑定 ──
        _bindEvents();
    }

    function _bindEvents() {
 // Tab 切换
        var tabBtns = document.querySelectorAll('#view-memory .memory-tab');
        for (var i = 0; i < tabBtns.length; i++) {
            tabBtns[i].addEventListener('click', function () {
                var tab = this.getAttribute('data-tab');
                _switchTab(tab);
            });
        }

 // 人格筛选
        var personaSelect = $('memory-persona-select');
        if (personaSelect) {
            personaSelect.addEventListener('change', function () {
                _currentPersonaUid = this.value;
                _loadAllData();
                _updatePipelineButton();
            });
        }
    }

    function _switchTab(tab) {
        _activeTab = tab;

 // 更新 Tab 激活态
        var allTabs = document.querySelectorAll('#view-memory .memory-tab');
        for (var i = 0; i < allTabs.length; i++) {
            allTabs[i].classList.toggle('active', allTabs[i].getAttribute('data-tab') === tab);
        }

 // 切换面板
        var allPanels = document.querySelectorAll('#view-memory .memory-panel');
        for (var j = 0; j < allPanels.length; j++) {
            allPanels[j].classList.toggle('active', allPanels[j].getAttribute('data-panel') === tab);
        }
    }

 // =========================================================
 // 数据加载
 // =========================================================

    async function _loadAllData() {
        // 显示加载状态（直接在各面板内展示，不用骨架屏避免 innerHTML 覆盖问题）
        _showPanelLoading();

        try {
            // 并行加载 L1/L2/L3 画像 + 画像状态
            // L3 改用 get_personality_profile（含完整 TraitDetailView 字段），
            // 而非 get_l3_traits（不含 trigger/suppress/not_meaning/related/seq）
            var results = await Promise.allSettled([
                RamariaApi.memory.getL1(_currentPersonaUid, 500),
                RamariaApi.memory.getL2(_currentPersonaUid, 500),
                RamariaApi.memory.getProfile(_currentPersonaUid),
                RamariaApi.memory.getProfileStatus(_currentPersonaUid),
            ]);

            var l1Data = results[0].status === 'fulfilled' ? results[0].value : [];
            var l2Data = results[1].status === 'fulfilled' ? results[1].value : [];
            var profile = results[2].status === 'fulfilled' ? results[2].value : null;
            var profileStatus = results[3].status === 'fulfilled' ? results[3].value : null;

            // 移除 L1 降级全量查询。
            // 原 fallback（getL1(null)）导致系统人格（rama-0001/user-0001）
            // 和按 persona 过滤无结果的 persona 错误显示全量导入 L1 数据。
            // 若某 persona 的 L1 为空，显示"暂无 L1 摘要"是正确行为。

            // 将 get_personality_profile 的三层结构展平为带 layer 标记的数组
            var l3Data = [];
            if (profile) {
                (profile.base || []).forEach(function(t) { t.layer = 'base'; l3Data.push(t); });
                (profile.primary || []).forEach(function(t) { t.layer = 'primary'; l3Data.push(t); });
                (profile.accent || []).forEach(function(t) { t.layer = 'accent'; l3Data.push(t); });
            }

            // 缓存
            _cache[_currentPersonaUid] = { l1: l1Data, l2: l2Data, l3: l3Data, profileStatus: profileStatus };

            // 渲染
            _renderL1(l1Data || []);
            _renderL2(l2Data || []);
            _renderL3(l3Data || [], profileStatus);

            // 更新 badge 数量
            _updateBadges(l1Data, l2Data, l3Data);

            // 错误日志
            if (results[0].status === 'rejected') console.error('[MemoryView] L1 加载失败:', results[0].reason);
            if (results[1].status === 'rejected') console.error('[MemoryView] L2 加载失败:', results[1].reason);
            if (results[2].status === 'rejected') console.error('[MemoryView] L3 画像加载失败:', results[2].reason);
            if (results[3].status === 'rejected') console.error('[MemoryView] 画像状态加载失败:', results[3].reason);

        } catch (err) {
            console.error('[MemoryView] 加载数据失败:', err);
            RamariaToast.show('error', '加载记忆失败', err.message || '未知错误');
            // 加载失败时在各面板显示错误提示
            _showPanelError(err.message || '未知错误');
        }
    }

 /** 在各面板显示加载中状态 */
    function _showPanelLoading() {
        var panels = ['memory-panel-l1', 'memory-panel-l2', 'memory-panel-l3'];
        for (var i = 0; i < panels.length; i++) {
            var panel = document.getElementById(panels[i]);
            if (panel) {
                panel.innerHTML =
                    '<div class="memory-loading">' +
                        '<span class="memory-loading-dot"></span>' +
                        '<span class="memory-loading-dot"></span>' +
                        '<span class="memory-loading-dot"></span>' +
                        ' 加载中...' +
                    '</div>';
            }
        }
    }

 /** 在各面板显示错误提示 */
    function _showPanelError(msg) {
        var panels = ['memory-panel-l1', 'memory-panel-l2', 'memory-panel-l3'];
        for (var i = 0; i < panels.length; i++) {
            var panel = document.getElementById(panels[i]);
            if (panel) {
                panel.innerHTML =
                    '<div class="memory-empty">' +
                        '<div class="memory-empty-icon">⚠</div>' +
                        '<div class="memory-empty-text">加载失败<br>' + msg + '</div>' +
                    '</div>';
            }
        }
    }

 // =========================================================
 // L1 渲染 — Bento Grid
 // =========================================================

    function _renderL1(items) {
        var panel = $('memory-panel-l1');
        if (!panel) return;
        panel.innerHTML = '';

        if (!items || items.length === 0) {
            panel.innerHTML =
                '<div class="memory-empty">' +
                    '<div class="memory-empty-icon">📄</div>' +
                    '<div class="memory-empty-text">暂无 L1 会话摘要<br>开始对话后自动生成</div>' +
                '</div>';
            return;
        }

        var grid = document.createElement('div');
        grid.className = 'memory-l1-grid';

        for (var i = 0; i < items.length; i++) {
            let item = items[i];

            // 解析 context_json 获取扩展字段
            var ctx = null;
            var hasCtx = false;
            try {
                if (item.context_json) {
                    ctx = typeof item.context_json === 'string'
                        ? JSON.parse(item.context_json) : item.context_json;
                    hasCtx = true;
                }
            } catch (_) { /* 解析失败，按无 context_json 降级 */ }

            // 从顶层字段 + context_json 提取扩展字段
            // time_period 优先从 L1 顶层字段读取，回退 context_json
            var timePeriod = item.time_period || (ctx && ctx.time_period) || '';
            var chatPartners = (ctx && ctx.chat_partners && ctx.chat_partners.length > 0)
                ? ctx.chat_partners : [];
            var msgCount = (ctx && ctx.message_count) ? ctx.message_count : '';
            // L1 顶层字段
            var atmosphere = item.atmosphere || '';
            var valence = (item.valence != null) ? item.valence : 0;
            var keywords = item.keywords || '';
            let hasSession = item.session_id && item.session_id.length > 0;

            // 确定 valence CSS class
            var valenceClass;
            if (valence > 0.3) {
                valenceClass = 'memory-l1-valence--positive';
            } else if (valence < -0.3) {
                valenceClass = 'memory-l1-valence--negative';
            } else {
                valenceClass = 'memory-l1-valence--neutral';
            }

            var card = document.createElement('div');
            card.className = 'memory-l1-card ' + valenceClass;
            card.setAttribute('data-l1-id', item.id || '');

            // 构建关键词 chip 标签
            var chipsHtml = '';
            if (keywords) {
                var kwList = keywords.split(',');
                for (var k = 0; k < kwList.length; k++) {
                    var kw = kwList[k].trim();
                    if (kw) {
                        chipsHtml += '<span class="memory-l1-chip">' + _escapeHtml(kw) + '</span>';
                    }
                }
            }

            // 构建属性行
            var attrsHtml = '';
            var attrParts = [];
            if (timePeriod) {
                attrParts.push('<span class="memory-l1-attr">🕐 ' + _escapeHtml(timePeriod) + '</span>');
            }
            if (atmosphere) {
                var valenceEmoji = _valenceEmoji(valence);
                attrParts.push('<span class="memory-l1-attr">' + valenceEmoji + ' ' + _escapeHtml(atmosphere) + '</span>');
            }
            // 旧卡片兼容：有 context_json 才显示参与人数；否则隐藏
            if (hasCtx && chatPartners.length > 0) {
                attrParts.push('<span class="memory-l1-attr">👥 ' + chatPartners.length + '人</span>');
            }
            if (attrParts.length > 0) {
                attrsHtml = '<div class="memory-l1-card-attrs">' + attrParts.join('') + '</div>';
            } else if (atmosphere) {
                // 旧卡片降级：有氛围但无 context_json，仅显示氛围
                var valenceEmojiOld = _valenceEmoji(valence);
                attrsHtml = '<div class="memory-l1-card-attrs">' +
                    '<span class="memory-l1-attr">' + valenceEmojiOld + ' ' + _escapeHtml(atmosphere) + '</span>' +
                    '</div>';
            }
            // 若仍为空则不渲染属性行

            card.innerHTML =
                // ── valence 色条（通过 ::before 伪元素渲染，此处仅占位标记）──
                // ── 标题 ──
                '<div class="memory-l1-card-header">' +
                    '<div class="memory-l1-card-title">' + _escapeHtml(item.summary || '(无摘要)') + '</div>' +
                '</div>' +
                // ── 属性行（时段 | 氛围 | 参与人数）──
                attrsHtml +
                // ── 关键词 chips ──
                (chipsHtml ? '<div class="memory-l1-card-chips">' + chipsHtml + '</div>' : '') +
                // ── 底部操作栏：时间 + 跳转按钮 ──
                '<div class="memory-l1-card-actions">' +
                    '<span class="memory-l1-card-time">' +
                        RamariaFormat.smartTime(item.created_at) +
                    '</span>' +
                    // 强度条保留在中间
                    '<div class="memory-salience-bar">' +
                        '<span class="memory-salience-label">强度</span>' +
                        '<div class="memory-salience-track">' +
                            '<div class="memory-salience-fill memory-salience-fill-w' +
                                (Math.round((item.salience || 0) * 10) * 10) + '"></div>' +
                        '</div>' +
                    '</div>' +
                    (hasSession
                        ? '<button class="memory-l1-view-chat-btn" data-session-id="' + item.session_id + '" ' +
                            'data-persona-uid="' + (item.persona_uid || '') + '" ' +
                            'title="跳转到该会话查看完整对话" aria-label="查看对话">' +
                            '💬 查看对话' + (msgCount ? ' (' + msgCount + ' 条消息)' : '') +
                            ' →</button>'
                        : '<span class="memory-l1-no-session-hint">会话已过期</span>') +
                '</div>';

            // 卡片整体点击 → 展开详情 Modal（保留旧行为）
            card.addEventListener('click', function (e) {
                // 如果点击目标是"查看对话"按钮，不弹 Modal
                if (e.target.closest && e.target.closest('.memory-l1-view-chat-btn')) {
                    return;
                }

                var detail = (item.summary || '') + '\n\n' +
                    '氛围: ' + (item.atmosphere || '-') + '\n' +
                    '效价: ' + (item.valence != null ? item.valence.toFixed(2) : '-') + '\n' +
                    '显著性: ' + (item.salience != null ? (item.salience * 100).toFixed(0) + '%' : '-') + '\n' +
                    '关键词: ' + (item.keywords || '-') + '\n' +
                    '人格: ' + (item.persona_uid || '-') + '\n' +
                    '会话: ' + (hasSession ? item.session_id.substring(0, 8) + '...' : '-');

                RamariaModal.show({
                    title: 'L1 摘要详情',
                    body: '<pre class="memory-modal-pre">' +
                          (RamariaMarkdown ? RamariaMarkdown.sanitize(detail) : detail) +
                          '</pre>',
                    footer: '<button class="btn btn-secondary" data-action="close">关闭</button>',
                });
            });

            // "查看对话"按钮独立事件（阻止冒泡）
            var viewChatBtn = card.querySelector('.memory-l1-view-chat-btn');
            if (viewChatBtn) {
                viewChatBtn.addEventListener('click', function (e) {
                    e.stopPropagation();

                    var sessionId = this.getAttribute('data-session-id');
                    var personaUid = this.getAttribute('data-persona-uid') || _currentPersonaUid;

                    if (!sessionId) {
                        RamariaToast.show('warning', '无法跳转', '该摘要未关联有效会话');
                        return;
                    }

                    console.log('[MemoryView] L1 卡片跳转对话: session=' + sessionId.substring(0, 8) +
                        ', persona=' + personaUid);

                    // 在离开前保存 L1 面板滚动位置
                    var l1Panel = document.getElementById('memory-panel-l1');
                    var scrollTop = l1Panel ? l1Panel.scrollTop : 0;

                    _savedState = {
                        personaUid: _currentPersonaUid,
                        activeTab: _activeTab,
                        l1ScrollTop: scrollTop, // 保存滚动位置
                    };
                    _returningFromChat = true;

                    RamariaRouter.showView('chat', {
                        sessionId: sessionId,
                        personaUid: personaUid,
                        fromView: 'memory',
                    });
                });
            }

            grid.appendChild(card);
        }

        panel.appendChild(grid);
    }

    /**
     * HTML 文本转义（防 XSS）。
     */
    function _escapeHtml(text) {
        if (!text) return '';
        var div = document.createElement('div');
        div.appendChild(document.createTextNode(String(text)));
        return div.innerHTML;
    }

    function _valenceEmoji(valence) {
        if (valence == null) return '';
        if (valence > 0.3) return '😊';
        if (valence < -0.3) return '😢';
        return '😐';
    }

 // =========================================================
 // L2 渲染 — 事件列表
 // =========================================================

    function _renderL2(items) {
        var panel = $('memory-panel-l2');
        if (!panel) return;
        panel.innerHTML = '';

        if (!items || items.length === 0) {
            panel.innerHTML =
                '<div class="memory-empty">' +
                    '<div class="memory-empty-icon">📋</div>' +
                    '<div class="memory-empty-text">暂无 L2 事件<br>累积足够 L1 摘要后自动提取</div>' +
                '</div>';
            return;
        }

        var list = document.createElement('div');
        list.className = 'memory-l2-list';

        for (var i = 0; i < items.length; i++) {
            var item = items[i];
            var confidencePct = item.confidence != null ? Math.round(item.confidence * 100) + '%' : '—';
            var shareLabel = item.share != null ? Math.round(item.share * 100) + '%' : '—';

            var li = document.createElement('div');
            li.className = 'memory-l2-item';

            li.innerHTML =
                '<div class="memory-l2-item-header">' +
                    '<div class="memory-l2-item-marker"></div>' +
                    '<div class="memory-l2-item-title">' + (item.title || item.summary || '(无标题)') + '</div>' +
                    '<div class="memory-l2-item-meta">' +
                        '<span class="memory-l2-item-tag">置信 ' + confidencePct + '</span>' +
                        '<span class="memory-l2-item-tag">共享 ' + shareLabel + '</span>' +
                    '</div>' +
                '</div>' +
                '<div class="memory-l2-item-detail">' +
                    '<div class="memory-l2-detail-row">' +
                        '<span class="memory-l2-detail-label">摘要</span>' +
                        '<span class="memory-l2-detail-value">' + (item.summary || '-') + '</span>' +
                    '</div>' +
                    '<div class="memory-l2-detail-row">' +
                        '<span class="memory-l2-detail-label">效价</span>' +
                        '<span class="memory-l2-detail-value">' + (item.valence != null ? item.valence.toFixed(2) : '-') + '</span>' +
                    '</div>' +
                    '<div class="memory-l2-detail-row">' +
                        '<span class="memory-l2-detail-label">表现</span>' +
                        '<span class="memory-l2-detail-value">' + (item.presentation || '-') + '</span>' +
                    '</div>' +
                    '<div class="memory-l2-detail-row">' +
                        '<span class="memory-l2-detail-label">态度</span>' +
                        '<span class="memory-l2-detail-value">' + (item.attitude || '-') + '</span>' +
                    '</div>' +
                    '<div class="memory-l2-detail-row">' +
                        '<span class="memory-l2-detail-label">关键词</span>' +
                        '<span class="memory-l2-detail-value">' + (item.keywords || '-') + '</span>' +
                    '</div>' +
                    '<div class="memory-l2-detail-row">' +
                        '<span class="memory-l2-detail-label">时间</span>' +
                        '<span class="memory-l2-detail-value">' + RamariaFormat.smartTime(item.created_at) + '</span>' +
                    '</div>' +
                '</div>';

            li.querySelector('.memory-l2-item-header').addEventListener('click', function () {
                this.parentElement.classList.toggle('expanded');
            });

            list.appendChild(li);
        }

        panel.appendChild(list);
    }

// =========================================================
// L3 渲染 — 三层性格画像
// =========================================================

/**
 * 渲染 L3 性格画像面板。
 *
 * 参数:
 * - `items`: L3 trait 数组（来自 getL3Traits API）。
 * - `profileStatus`: 画像数据状态对象（来自 getProfileStatus API），
 *    含 n_total_eff / active_trait_count / status / status_text。
 */
    function _renderL3(items, profileStatus) {
        var panel = $('memory-panel-l3');
        if (!panel) return;
        panel.innerHTML = '';

        if (!items || items.length === 0) {
            _renderL3Empty(panel);
            return;
        }

        // 构建容器
        var container = document.createElement('div');
        container.className = 'memory-l3-profile';

        // ── 状态指示器 ──
        if (profileStatus) {
            container.appendChild(_buildStatusBar(profileStatus));
        }

        // ── 按 layer 分组 ──
        var groups = { base: [], primary: [], accent: [] };
        for (var i = 0; i < items.length; i++) {
            var layer = items[i].layer || 'accent';
            if (!groups[layer]) groups[layer] = [];
            groups[layer].push(items[i]);
        }

        var layerOrder = ['base', 'primary', 'accent'];
        var layerConfig = {
            base:    { title: '底色层', subtitle: '跨情境稳定的深层性格基调', icon: '🏛️' },
            primary: { title: '主色调层', subtitle: '日常最突出的性格特征', icon: '🎨' },
            accent:  { title: '点缀层', subtitle: '特定条件下浮现的性格侧面', icon: '✨' },
        };

        for (var l = 0; l < layerOrder.length; l++) {
            var layer = layerOrder[l];
            var traits = groups[layer] || [];
            var cfg = layerConfig[layer];

            var section = document.createElement('div');
            section.className = 'memory-l3-section memory-l3-section--' + layer;

            // 层标题
            section.innerHTML =
                '<div class="memory-l3-section-header">' +
                    '<span class="memory-l3-section-icon">' + cfg.icon + '</span>' +
                    '<div class="memory-l3-section-text">' +
                        '<div class="memory-l3-section-title">' + cfg.title + '</div>' +
                        '<div class="memory-l3-section-subtitle">' + cfg.subtitle + '</div>' +
                    '</div>' +
                    '<span class="memory-l3-section-count">' + traits.length + '</span>' +
                '</div>';

            // trait 列表
            var traitList = document.createElement('div');
            traitList.className = 'memory-l3-traits';

            // 层内按 seq 排序
            traits.sort(function (a, b) { return (a.seq || 0) - (b.seq || 0); });

            for (var t = 0; t < traits.length; t++) {
                traitList.appendChild(_buildTraitCard(traits[t], layer));
            }

            // 空层时显示提示
            if (traits.length === 0) {
                var emptyHint = document.createElement('div');
                emptyHint.className = 'memory-l3-traits-empty';
                emptyHint.textContent = '该层暂无性格标签';
                traitList.appendChild(emptyHint);
            }

            section.appendChild(traitList);
            container.appendChild(section);
        }

        panel.appendChild(container);
    }

// =========================================================
// L3 空状态
// =========================================================

    function _renderL3Empty(panel) {
        panel.innerHTML =
            '<div class="memory-empty">' +
                '<div class="memory-empty-icon">🏷️</div>' +
                '<div class="memory-empty-text">暂无 L3 性格画像<br>积累足够事件后自动推断</div>' +
            '</div>';
    }

// =========================================================
// 状态指示器
// =========================================================

    function _buildStatusBar(status) {
        var bar = document.createElement('div');
        bar.className = 'memory-l3-status-bar';

        var statusIcon = { insufficient: '🔴', preliminary: '🟡', trusted: '🟢' };
        var icon = statusIcon[status.status] || '⚪';

        bar.innerHTML =
            '<span class="memory-l3-status-icon">' + icon + '</span>' +
            '<span class="memory-l3-status-text">' + _escapeHtml(status.status_text || '') + '</span>';

        return bar;
    }

// =========================================================
// 单条 trait 卡片
// =========================================================

    function _buildTraitCard(trait, layer) {
        var card = document.createElement('div');
        card.className = 'memory-l3-trait-card';
        card.setAttribute('data-trait-id', trait.id || '');

        var confidencePct = trait.confidence != null ? Math.round(trait.confidence * 100) : 0;
        var confClass = confidencePct >= 80 ? 'high' : (confidencePct >= 60 ? 'mid' : 'low');

        // 头部: 标签名 + 置信度
        var html = '';
        html += '<div class="memory-l3-trait-header">';
        html += '<span class="memory-l3-trait-label">' + _escapeHtml(trait.label || '?') + '</span>';
        html += '<span class="memory-l3-trait-confidence ' + confClass + '">' + confidencePct + '% 置信</span>';
        html += '</div>';

        // 含义
        html += '<div class="memory-l3-trait-meaning">' + _escapeHtml(trait.meaning || '') + '</div>';

        // 置信度色条
        html += '<div class="memory-l3-confidence-bar">';
        html += '<div class="memory-l3-confidence-fill ' + confClass + '" style="width:' + Math.max(confidencePct, 2) + '%"></div>';
        html += '</div>';

        // 元信息行
        html += '<div class="memory-l3-trait-meta">';
        html += '<span>证据量: ' + (trait.evidence != null ? trait.evidence.toFixed(1) : '0') + '</span>';
        html += '<span>一致性: ' + (trait.consistency != null ? Math.round(trait.consistency * 100) + '%' : '-') + '</span>';
        html += '</div>';

        // 否定界定（如有）
        if (trait.not_meaning) {
            html += '<div class="memory-l3-trait-not">≠ ' + _escapeHtml(trait.not_meaning) + '</div>';
        }

        // 触发/抑制条件（仅 accent 层）
        if (layer === 'accent') {
            if (trait.trigger) {
                html += '<div class="memory-l3-trait-cond memory-l3-trait-cond--trigger">📌 触发: ' + _escapeHtml(trait.trigger) + '</div>';
            }
            if (trait.suppress) {
                html += '<div class="memory-l3-trait-cond memory-l3-trait-cond--suppress">🔇 抑制: ' + _escapeHtml(trait.suppress) + '</div>';
            }
        }

        // 关联性格（如有）
        if (trait.related) {
            html += '<div class="memory-l3-trait-related">🔗 ' + _escapeHtml(trait.related) + '</div>';
        }

        // 底部操作行
        html += '<div class="memory-l3-trait-footer">';
        html += '<span class="memory-l3-trait-time">' + RamariaFormat.smartTime(trait.created_at) + '</span>';
        html += '<button class="btn btn-sm btn-outline memory-l3-evidence-btn" ' +
                'data-trait-id="' + trait.id + '" ' +
                'data-trait-label="' + _escapeHtml(trait.label || '') + '">' +
                '📋 展开证据</button>';
        html += '</div>';

        // 证据链展开区（初始隐藏）
        html += '<div class="memory-l3-evidence-panel" id="evidence-panel-' + trait.id + '" style="display:none"></div>';

        card.innerHTML = html;

        // 绑定"展开证据"按钮
        _bindEvidenceButton(card, trait);

        return card;
    }

// =========================================================
// 证据链展开按钮
// =========================================================

    function _bindEvidenceButton(card, trait) {
        var btn = card.querySelector('.memory-l3-evidence-btn');
        if (!btn) return;

        btn.addEventListener('click', function (e) {
            e.stopPropagation();

            var panel = document.getElementById('evidence-panel-' + trait.id);
            if (!panel) return;

            // 切换展开/折叠
            if (panel.style.display !== 'none') {
                panel.style.display = 'none';
                panel.innerHTML = '';
                btn.textContent = '📋 展开证据';
                return;
            }

            panel.style.display = 'block';
            btn.textContent = '📋 加载中...';

            // 使用证据链组件加载数据
            if (typeof RamariaTraitEvidence !== 'undefined') {
                // 增加调试日志，便于诊断首次展开空白问题。
                // trait.id=0 时 trait-evidence.js 内部会拦截并显示"暂未就绪"。
                console.debug('[MemoryView] 请求证据链: persona=' + _currentPersonaUid +
                    ', traitId=' + trait.id + ', label=' + (trait.label || '?'));

                // await render（异步）完成后恢复按钮文字
                RamariaTraitEvidence.render(panel, _currentPersonaUid, trait.id, trait.label || '?')
                    .then(function () {
                        console.debug('[MemoryView] 证据链渲染完成, traitId=' + trait.id);
                        btn.textContent = '📋 收起证据';
                    })
                    .catch(function (err) {
                        console.error('[MemoryView] 证据链加载失败, traitId=' + trait.id +
                            ', label=' + trait.label + ', err=', err);
                        btn.textContent = '📋 展开证据';
                        panel.innerHTML =
                            '<div class="tev-empty">' +
                                '<div class="tev-empty-text">证据链加载失败: ' + (err.message || '未知错误') + '</div>' +
                            '</div>';
                    });
            } else {
                panel.innerHTML =
                    '<div class="tev-empty">' +
                        '<div class="tev-empty-text">证据链组件未加载</div>' +
                    '</div>';
                btn.textContent = '📋 展开证据';
            }
        });
    }

// =========================================================
// 辅助函数
// =========================================================

    /** HTML 转义 */
    function _escapeHtml(str) {
        if (!str) return '';
        return String(str)
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;')
            .replace(/'/g, '&#39;');
    }

 // =========================================================
 // 辅助
 // =========================================================

    function _updateBadges(l1, l2, l3) {
        var badgeL1 = $('memory-badge-l1');
        var badgeL2 = $('memory-badge-l2');
        var badgeL3 = $('memory-badge-l3');

        if (badgeL1) badgeL1.textContent = l1 ? l1.length : '0';
        if (badgeL2) badgeL2.textContent = l2 ? l2.length : '0';
        if (badgeL3) badgeL3.textContent = l3 ? l3.length : '0';
    }

 // =========================================================
 // 人格选择器刷新
 // =========================================================

 /** 缓存的 persona 列表（含 source 字段，用于判断是否导入人格） */
    var _allPersonas = [];

    async function _refreshPersonaSelector() {
        var select = $('memory-persona-select');
        if (!select) return;

        try {
            var personas = await RamariaApi.memory.getPersonas();
            RamariaStore.set('personas', personas || []);
            _allPersonas = personas || [];

            select.innerHTML = '';
            for (var i = 0; i < _allPersonas.length; i++) {
                var opt = document.createElement('option');
                opt.value = _allPersonas[i].uid;
                opt.textContent = _allPersonas[i].name + ' (' + _allPersonas[i].uid + ')';
                select.appendChild(opt);
            }

 // ── 检查是否有预设的 persona（来自导入完成页的导航）──
            var preselectUid = RamariaStore.get('preselectPersonaUid');
            if (preselectUid) {
                var preselectOpt = select.querySelector('option[value="' + preselectUid + '"]');
                if (preselectOpt) {
                    select.value = preselectUid;
                    _currentPersonaUid = preselectUid;
                    console.log('[MemoryView] 预设 persona 选择器: ' + preselectUid);
                } else if (_allPersonas.length > 0) {
                    _currentPersonaUid = _allPersonas[0].uid;
                }
 // 清除预设标志（仅生效一次）
                RamariaStore.set('preselectPersonaUid', null);
            } else {
 // 默认 rama-0001
                var def = select.querySelector('option[value="rama-0001"]');
                if (def) {
                    select.value = 'rama-0001';
                    _currentPersonaUid = 'rama-0001';
                } else if (_allPersonas.length > 0) {
                    select.value = _allPersonas[0].uid;
                    _currentPersonaUid = _allPersonas[0].uid;
                }
            }
        } catch (err) {
            console.error('[MemoryView] 加载人格列表失败:', err);
        }
    }

 // =========================================================
 // 深度处理按钮
 // =========================================================

 /** 管道是否正在执行中（防止重复点击） */
    var _pipelineRunning = false;
 /** 按钮事件是否已绑定 */
    var _pipelineBtnBound = false;

 /**
 * 根据当前选中的 persona 决定是否显示"深度处理"按钮。
 * 仅当 persona.source === "qq"（导入人格）时显示。
 */
    function _updatePipelineButton() {
        var btn = document.getElementById('btn-trigger-pipeline');
        if (!btn) return;

 // 查找当前选中 persona 的 source
        var currentPersona = null;
        for (var i = 0; i < _allPersonas.length; i++) {
            if (_allPersonas[i].uid === _currentPersonaUid) {
                currentPersona = _allPersonas[i];
                break;
            }
        }

        var isImported = currentPersona && currentPersona.source === 'qq';
        if (isImported) {
            btn.classList.remove('hidden');
        } else {
            btn.classList.add('hidden');
        }

 // 首次绑定点-击事件
        if (!_pipelineBtnBound) {
            btn.addEventListener('click', _handleTriggerPipeline);
            _pipelineBtnBound = true;
        }
    }

 /**
 * 处理"深度处理导入的消息"按钮点击。
 *
 * 调用新命令 `regenerate_import_pipeline`，先重新生成 L1 摘要，
 * 再自动级联 L2→L3。覆盖导入时 LLM 不可用导致 L1 失败的场景。
 *
 * 流程:
 * 1. 后端查找该 persona 的所有关联 session
 * 2. 对每个 session 重新生成 L1 摘要（persona_uid=NULL，幂等覆盖）
 * 3. L1 全部完成后触发 L2→L3 级联（后台异步）
 */
    async function _handleTriggerPipeline() {
        if (_pipelineRunning) {
            RamariaToast.show('warning', '提示', '深度处理正在进行中，请稍候...');
            return;
        }

        if (!_currentPersonaUid) {
            RamariaToast.show('warning', '提示', '请先选择一个导入人格');
            return;
        }

        var btn = document.getElementById('btn-trigger-pipeline');
        _pipelineRunning = true;
        if (btn) {
            btn.disabled = true;
            btn.textContent = '⏳ 处理中...';
        }

        try {
            var result = await RamariaApi.memory.regenerateImportPipeline(_currentPersonaUid);

 // 解析结果
            var l1Regenerated = (result && result.l1_regenerated) ? result.l1_regenerated : 0;
            var l1Failed = (result && result.l1_failed) ? result.l1_failed : 0;
            var totalSessions = (result && result.total_sessions) ? result.total_sessions : 0;
            var earlyTerminated = !!(result && result.early_terminated);
            var remainingSkipped = (result && result.remaining_skipped) ? result.remaining_skipped : 0;

 // ── 区分提前终止 vs 部分失败 vs 全部成功 ──
            if (earlyTerminated) {
 // 连续失败达上限，已提前终止 → 用 error 级 toast 强调需要用户操作
                RamariaToast.show(
                    'error', 'LLM 不可用，已提前终止',
                    '连续失败 ' + l1Failed + ' 次，已跳过剩余 ' + remainingSkipped + ' 个 session。' +
                    '请确认 LLM 模型已连接后重试。',
                    { duration: 10000 }
                );
            } else if (l1Failed > 0) {
                RamariaToast.show(
                    'warning', '部分失败',
                    'L1 重新生成: 成功 ' + l1Regenerated + '/' + totalSessions +
                    '，失败 ' + l1Failed + '。请确认 LLM 模型连接正常后重试。L2/L3 将在后台继续处理。',
                    { duration: 8000 }
                );
            } else if (l1Regenerated > 0) {
                RamariaToast.show(
                    'success', '处理完成',
                    'L1 全部重新生成成功 (' + l1Regenerated + '/' + totalSessions +
                    ')。L2 事件提取和 L3 性格画像将在后台异步执行。',
                    { duration: 5000 }
                );
            } else {
                RamariaToast.show('info', '提示', '该人格没有关联的消息需要处理。');
            }

 // 延迟刷新记忆数据
            setTimeout(function () {
                _loadAllData();
                _pipelineRunning = false;
                if (btn) {
                    btn.disabled = false;
                    btn.textContent = '🔬 深度处理导入的消息';
                }
            }, 5000);
        } catch (err) {
            _pipelineRunning = false;
            if (btn) {
                btn.disabled = false;
                btn.textContent = '🔬 深度处理导入的消息';
            }
            console.error('[MemoryView] 深度处理失败:', err);
            RamariaToast.show('error', '处理失败', err.message || String(err));
        }
    }

 // =========================================================
 // 生命周期
 // =========================================================

    function _registerHooks() {
        var unreg;

        // enter 钩子接收 Router options（第二个参数）
        unreg = RamariaRouter.registerHook('memory', 'enter', function (_viewName, options) {
            console.log('[MemoryView] 进入视图');

            // 检查是否从对话页返回（需要恢复状态）
            var shouldRestore = _returningFromChat && _savedState;
            // 在清除 _savedState 前提取滚动位置
            var savedScrollTop = 0;
            if (shouldRestore) {
                savedScrollTop = _savedState.l1ScrollTop || 0;
                console.log('[MemoryView] 从对话页返回，恢复状态: persona=' +
                    _savedState.personaUid + ', tab=' + _savedState.activeTab +
                    ', scrollTop=' + savedScrollTop);
            }

            render();

            _refreshPersonaSelector().then(function () {
                // 从对话页返回时恢复之前的 persona 和 tab
                if (shouldRestore) {
                    if (_savedState.personaUid) {
                        var select = $('memory-persona-select');
                        if (select) {
                            var opt = select.querySelector('option[value="' + _savedState.personaUid + '"]');
                            if (opt) {
                                select.value = _savedState.personaUid;
                                _currentPersonaUid = _savedState.personaUid;
                            }
                        }
                    }
                    if (_savedState.activeTab) {
                        _activeTab = _savedState.activeTab;
                        _switchTab(_savedState.activeTab);
                    }
                    // 清除标记
                    _returningFromChat = false;
                    _savedState = null;
                }

                _loadAllData().then(function () {
                    // 从对话页返回时恢复 L1 滚动位置
                    if (shouldRestore && savedScrollTop > 0) {
                        // 使用 requestAnimationFrame 确保 DOM 已布局完成
                        requestAnimationFrame(function () {
                            var l1Panel = document.getElementById('memory-panel-l1');
                            if (l1Panel) {
                                l1Panel.scrollTop = savedScrollTop;
                                console.log('[MemoryView] 恢复 L1 滚动位置: ' + savedScrollTop);
                            }
                        });
                    }
                });

                _updatePipelineButton();
            });
        });
        _unregisterFns.push(unreg);

        unreg = RamariaRouter.registerHook('memory', 'leave', function () {
            console.log('[MemoryView] 离开视图');

            // 离开时保存状态快照（仅在前往对话页时由 L1 卡片按钮设置 _returningFromChat）
            // 若 _returningFromChat 为 true（即将跳转到对话页），保存当前状态
            if (_returningFromChat) {
                _savedState = {
                    personaUid: _currentPersonaUid,
                    activeTab: _activeTab,
                };
            }

            for (var i = 0; i < _unsubs.length; i++) {
                try { _unsubs[i](); } catch (_) { /* ignore */ }
            }
            _unsubs = [];
        });
        _unregisterFns.push(unreg);
    }

    function init() {
        console.log('[MemoryView] 初始化记忆查看视图...');
        _registerHooks();
    }

 // =========================================================
 // 公开 API
 // =========================================================

    return {
        init: init,
        destroy: function () {
            for (var i = 0; i < _unregisterFns.length; i++) {
                try { _unregisterFns[i](); } catch (_) { /* ignore */ }
            }
            _unregisterFns = [];
            for (var j = 0; j < _unsubs.length; j++) {
                try { _unsubs[j](); } catch (_) { /* ignore */ }
            }
            _unsubs = [];
            _cache = {};
            console.log('[MemoryView] 已销毁');
        },
    };
})();

// 自动初始化
(function _autoInit() {
    if (typeof RamariaRouter === 'undefined') {
        setTimeout(_autoInit, 50);
        return;
    }
    RamariaMemoryView.init();

    var currentView = RamariaRouter.getCurrentView();
    if (currentView === 'memory') {
        setTimeout(function () {
            if (RamariaRouter.getCurrentView() === 'memory') {
                RamariaRouter.showView('memory', { forceReenter: true });
            }
        }, 10);
    }
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaMemoryView', {
    value: RamariaMemoryView,
    writable: false,
    configurable: false,
});
