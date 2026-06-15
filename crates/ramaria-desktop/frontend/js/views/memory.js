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

        // ── v1.1 修复: render() 会重建 DOM，旧的按钮监听器随之销毁 ──
        // 重置标志确保 _updatePipelineButton() 在新 DOM 上重新绑定 click 事件。
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
            // 并行加载三层数据
            var results = await Promise.allSettled([
                RamariaApi.memory.getL1(_currentPersonaUid, 500),
                RamariaApi.memory.getL2(_currentPersonaUid, 500),
                RamariaApi.memory.getL3(_currentPersonaUid),
            ]);

            var l1Data = results[0].status === 'fulfilled' ? results[0].value : [];
            var l2Data = results[1].status === 'fulfilled' ? results[1].value : [];
            var l3Data = results[2].status === 'fulfilled' ? results[2].value : [];

            // v1.1 降级：若按 persona 过滤无结果，尝试不过滤再查一次
            if (l1Data.length === 0 && _currentPersonaUid) {
                try {
                    var l1Fallback = await RamariaApi.memory.getL1(null, 500);
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
                        '<span class="memory-salience-label">强度</span>' +
                        '<div class="memory-salience-track">' +
                            '<div class="memory-salience-fill memory-salience-fill-w' +
                                (Math.round((item.salience || 0) * 10) * 10) + '"></div>' +
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
                    body: '<pre class="memory-modal-pre">' +
                          (RamariaMarkdown ? RamariaMarkdown.sanitize(detail) : detail) +
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
            section.innerHTML = '<div class="memory-l3-section-title">' + layerNames[layer] + '</div>';

            var tags = document.createElement('div');
            tags.className = 'memory-l3-cloud';

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
                            body: '<pre class="memory-modal-pre">' +
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

            // ── v1.1 修复: 检查是否有预设的 persona（来自导入完成页的导航）──
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
     * v1.1 修复: 调用新命令 `regenerate_import_pipeline`，先重新生成 L1 摘要，
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

            // ── v1.1 修复: 区分提前终止 vs 部分失败 vs 全部成功 ──
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

        unreg = RamariaRouter.registerHook('memory', 'enter', function () {
            console.log('[MemoryView] 进入视图');
            render();
            _refreshPersonaSelector().then(function () {
                _loadAllData();
                _updatePipelineButton();
            });
        });
        _unregisterFns.push(unreg);

        unreg = RamariaRouter.registerHook('memory', 'leave', function () {
            console.log('[MemoryView] 离开视图');
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
