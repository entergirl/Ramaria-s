/**
 * js/views/memory.js — Ramaria 记忆查看视图
 *
 * 职责:
 * - 三 Tab 记忆查看：L1 会话摘要（Bento Grid）/ L2 事件列表 / L3 性格标签云
 * - 按人格筛选记忆（PersonaSelector）
 * - 每层独立数据加载和缓存，切换 Tab 不重新请求
 * - 点击卡片/事件项展开详情
 *
 * 设计特点:
 * - 注册 Router enter/leave 钩子
 * - enter 时自动加载当前人格的 L1/L2/L3 数据
 * - 三面板通过 .active 类切换显示，DOM 始终保留（避免重复渲染）
 * - L1 Bento Grid：粉色渐变卡片，显示 salience 强度条
 * - L2 事件列表：蓝色主题，点击展开详情（源/态度/置信度等）
 * - L3 标签云：base=粉色大字 / primary=蓝色中字 / accent=灰色小字，hover 放大
 * - 空数据友好提示
 *
 * 依赖:
 * - RamariaApi / RamariaStore / RamariaRouter
 * - RamariaToast / RamariaSkeleton / RamariaFormat
 * - RamariaModal
 * - CSS: css/views/memory.css
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
            '<span style="font-size:11px;color:var(--text-tertiary);">筛选记忆归属人格</span>';
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
            // 并行加载三层数据
            var results = await Promise.allSettled([
                RamariaApi.memory.getL1(_currentPersonaUid, 100),
                RamariaApi.memory.getL2(_currentPersonaUid, 200),
                RamariaApi.memory.getL3(_currentPersonaUid),
            ]);

            var l1Data = results[0].status === 'fulfilled' ? results[0].value : [];
            var l2Data = results[1].status === 'fulfilled' ? results[1].value : [];
            var l3Data = results[2].status === 'fulfilled' ? results[2].value : [];

            // v1.1 降级：若按 persona 过滤无结果，尝试不过滤再查一次
            if (l1Data.length === 0 && _currentPersonaUid) {
                try {
                    var l1Fallback = await RamariaApi.memory.getL1(null, 100);
                    if (l1Fallback && l1Fallback.length > 0) {
                        console.warn('[MemoryView] L1 按 persona=' + _currentPersonaUid + ' 查询为空，降级为全量查询，找到 ' + l1Fallback.length + ' 条');
                        l1Data = l1Fallback;
                    }
                } catch (_) { /* 降级查询失败，保持空结果 */ }
            }

            // 缓存
            _cache[_currentPersonaUid] = { l1: l1Data, l2: l2Data, l3: l3Data };

            // 渲染
            _renderL1(l1Data || []);
            _renderL2(l2Data || []);
            _renderL3(l3Data || []);

            // 更新 badge 数量
            _updateBadges(l1Data, l2Data, l3Data);

            // 错误日志
            if (results[0].status === 'rejected') console.error('[MemoryView] L1 加载失败:', results[0].reason);
            if (results[1].status === 'rejected') console.error('[MemoryView] L2 加载失败:', results[1].reason);
            if (results[2].status === 'rejected') console.error('[MemoryView] L3 加载失败:', results[2].reason);

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
            var item = items[i];

            var card = document.createElement('div');
            card.className = 'memory-l1-card';
            card.setAttribute('data-l1-id', item.id || '');

            var valenceEmoji = _valenceEmoji(item.valence);
            var atmosphere = item.atmosphere || '';

            card.innerHTML =
                '<div class="memory-l1-card-header">' +
                    '<div class="memory-l1-card-title">' + (item.summary || '(无摘要)') + '</div>' +
                    (atmosphere
                        ? '<span class="memory-l1-card-badge">' + valenceEmoji + ' ' + atmosphere + '</span>'
                        : '') +
                '</div>' +
                '<div class="memory-l1-card-summary">' +
                    (item.keywords ? '🏷️ ' + item.keywords : '') +
                '</div>' +
                '<div class="memory-l1-card-footer">' +
                    '<span>' + RamariaFormat.smartTime(item.created_at) + '</span>' +
                    '<div class="memory-salience-bar">' +
                        '<span style="font-size:10px;">强度</span>' +
                        '<div class="memory-salience-track">' +
                            '<div class="memory-salience-fill" style="width:' +
                                Math.round((item.salience || 0) * 100) + '%"></div>' +
                        '</div>' +
                    '</div>' +
                '</div>';

            card.addEventListener('click', function () {
                var detail = (item.summary || '') + '\n\n' +
                    '氛围: ' + (item.atmosphere || '-') + '\n' +
                    '效价: ' + (item.valence != null ? item.valence.toFixed(2) : '-') + '\n' +
                    '显著性: ' + (item.salience != null ? (item.salience * 100).toFixed(0) + '%' : '-') + '\n' +
                    '关键词: ' + (item.keywords || '-') + '\n' +
                    '人格: ' + (item.persona_uid || '-') + '\n' +
                    '会话: ' + (item.session_id ? item.session_id.substring(0, 8) + '...' : '-');

                RamariaModal.show({
                    title: 'L1 摘要详情',
                    body: '<pre style="font-size:12.5px;line-height:1.7;color:var(--text-primary);' +
                          'white-space:pre-wrap;word-break:break-word;margin:0;font-family:var(--font-body);">' +
                          RamariaMarkdown ? RamariaMarkdown.sanitize(detail) : detail +
                          '</pre>',
                    footer: '<button class="btn btn-secondary" data-action="close">关闭</button>',
                });
            });

            grid.appendChild(card);
        }

        panel.appendChild(grid);
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
    // L3 渲染 — 性格标签云
    // =========================================================

    function _renderL3(items) {
        var panel = $('memory-panel-l3');
        if (!panel) return;
        panel.innerHTML = '';

        if (!items || items.length === 0) {
            panel.innerHTML =
                '<div class="memory-empty">' +
                    '<div class="memory-empty-icon">🏷️</div>' +
                    '<div class="memory-empty-text">暂无 L3 性格标签<br>积累足够事件后自动推断</div>' +
                '</div>';
            return;
        }

        // 按 layer 分组
        var groups = { base: [], primary: [], accent: [] };
        for (var i = 0; i < items.length; i++) {
            var layer = items[i].layer || 'accent';
            if (!groups[layer]) groups[layer] = [];
            groups[layer].push(items[i]);
        }

        // 排序：base → primary → accent
        var layerOrder = ['base', 'primary', 'accent'];
        var layerNames = { base: '底色（Base）', primary: '基调（Primary）', accent: '点缀（Accent）' };

        var cloud = document.createElement('div');
        cloud.className = 'memory-l3-cloud';

        for (var l = 0; l < layerOrder.length; l++) {
            var layer = layerOrder[l];
            var traits = groups[layer] || [];
            if (traits.length === 0) continue;

            var section = document.createElement('div');
            section.className = 'memory-l3-section';
            section.style.width = '100%';
            section.innerHTML = '<div class="memory-l3-section-title">' + layerNames[layer] + '</div>';

            var tags = document.createElement('div');
            tags.className = 'memory-l3-cloud';
            tags.style.justifyContent = 'flex-start';

            for (var t = 0; t < traits.length; t++) {
                // ★ 使用 let 声明以创建块级作用域闭包
                //    修复：var trait（函数作用域→所有回调共享最后一个值）
                //    let trait 确保每个 addEventListener 闭包捕获当次循环的值
                var _trait = traits[t];
                var _confidencePct = _trait.confidence != null ? Math.round(_trait.confidence * 100) : 0;

                var tag = document.createElement('button');
                tag.className = 'memory-l3-tag layer-' + layer;
                tag.textContent = _trait.label || _trait.meaning || '?';
                tag.title = (_trait.meaning || '') +
                    '\nnot: ' + (_trait.not_meaning || '-') +
                    '\n置信度: ' + _confidencePct + '%' +
                    '\n证据量: ' + (_trait.evidence || 0) +
                    '\n状态: ' + (_trait.status || 'active');

                // 通过 IIFE 绑定当前循环值，避免闭包引用循环变量
                (function (trait, confidencePct) {
                    tag.addEventListener('click', function () {
                        var detail =
                            '标签: ' + (trait.label || '-') + '\n' +
                            '含义: ' + (trait.meaning || '-') + '\n' +
                            '非含义: ' + (trait.not_meaning || '-') + '\n' +
                            '层次: ' + (trait.layer || '-') + '\n' +
                            '置信度: ' + confidencePct + '%\n' +
                            '证据量: ' + (trait.evidence || 0) + '\n' +
                            '一致性: ' + (trait.consistency != null ? (trait.consistency * 100).toFixed(0) + '%' : '-') + '\n' +
                            '状态: ' + (trait.status || 'active') + '\n' +
                            '触发: ' + (trait.trigger || '-') + '\n' +
                            '抑制: ' + (trait.suppress || '-') + '\n' +
                            '创建: ' + RamariaFormat.smartTime(trait.created_at);

                        RamariaModal.show({
                            title: '性格标签: ' + (trait.label || '?'),
                            body: '<pre style="font-size:12.5px;line-height:1.7;color:var(--text-primary);' +
                                  'white-space:pre-wrap;word-break:break-word;margin:0;font-family:var(--font-body);">' +
                                  (RamariaMarkdown ? RamariaMarkdown.sanitize(detail) : detail) +
                                  '</pre>',
                            footer: '<button class="btn btn-secondary" data-action="close">关闭</button>',
                        });
                    });
                })(_trait, _confidencePct);

                tags.appendChild(tag);
            }

            section.appendChild(tags);
            cloud.appendChild(section);
        }

        panel.appendChild(cloud);
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

    async function _refreshPersonaSelector() {
        var select = $('memory-persona-select');
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

            // 默认 rama-0001
            var def = select.querySelector('option[value="rama-0001"]');
            if (!def && personas.length > 0) {
                select.value = personas[0].uid;
                _currentPersonaUid = personas[0].uid;
            } else if (def) {
                select.value = 'rama-0001';
                _currentPersonaUid = 'rama-0001';
            }
        } catch (err) {
            console.error('[MemoryView] 加载人格列表失败:', err);
        }
    }

    // =========================================================
    // 生命周期
    // =========================================================

    function _registerHooks() {
        var unreg;

        unreg = RamariaRouter.registerHook('memory', 'enter', function () {
            console.log('[MemoryView] enter');
            render();
            _refreshPersonaSelector().then(function () {
                _loadAllData();
            });
        });
        _unregisterFns.push(unreg);

        unreg = RamariaRouter.registerHook('memory', 'leave', function () {
            console.log('[MemoryView] leave');
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
