/**
 * js/views/settings/index.js — Ramaria 设置页视图入口
 *
 * 职责:
 * - 视图框架渲染（基础/高级页签 + 左右布局 + 目录栏 + 内容区）
 * - 目录导航（点击切换 / 深链解析与同步 / 会话内选中项记忆 / 基础↔高级跳转）
 * - 生命周期钩子（enter 加载时序与原单文件实现一致）与公开 API 组装
 *
 * 设计特点:
 * - 各目录项由 NS.parts.* 提供 render / fill；本文件只做导航与数据加载编排
 * - 公开 API 与原单文件设置页逐项一致（回归测试直接驱动）
 * - 高级设置页签为"调试能力"，默认隐藏；启用调试并重启后可见（行为不变）
 * - 深链格式 `#settings/{tab}/{key}`，经 history.replaceState 同步（不监听 hashchange）
 *
 * 依赖:
 * - window.RamariaSettingsNS（shared.js 创建；各 section 模块注册 NS.parts）
 * - RamariaApi / RamariaStore / RamariaRouter
 */

var RamariaSettingsView = (function () {
    'use strict';

    var NS = window.RamariaSettingsNS;
    var state = NS.state;

 // =========================================================
 // 渲染
 // =========================================================

    function render() {
        var viewEl = NS.$('view-settings');
        if (!viewEl) {
            console.error('[SettingsView] 找不到 #view-settings 容器');
            return;
        }

        viewEl.innerHTML = '';

        var scroll = document.createElement('div');
        scroll.className = 'settings-scroll';
        viewEl.appendChild(scroll);

 // ── v1.4 M6：基础/高级两级 Tab 框架 ──
 // v2.0 M7：高级设置页签为"调试能力"，默认隐藏；设置页「启用调试」并重启后才显示
        state.debugEnabled = NS.currentDebugEnabled();
        var tabs = document.createElement('div');
        tabs.className = 'settings-tabs';
        tabs.innerHTML =
            '<button class="settings-tab-btn active" data-tab="basic">基础设置</button>' +
            (state.debugEnabled ? '<button class="settings-tab-btn" data-tab="advanced">高级设置</button>' : '');
        scroll.appendChild(tabs);

        var basicPane = document.createElement('div');
        basicPane.className = 'settings-tab-pane';
        basicPane.id = 'settings-pane-basic';
        _buildPaneLayout(basicPane, 'basic');
        scroll.appendChild(basicPane);

        var advancedPane = document.createElement('div');
        advancedPane.className = 'settings-tab-pane hidden';
        advancedPane.id = 'settings-pane-advanced';
        if (state.debugEnabled) _buildPaneLayout(advancedPane, 'advanced');
        scroll.appendChild(advancedPane);

 // ── Tab 切换绑定（切换时定位到该页签上次选中的目录项）──
        var tabBtns = tabs.querySelectorAll('.settings-tab-btn');
        for (var i = 0; i < tabBtns.length; i++) {
            tabBtns[i].addEventListener('click', (function (btn) {
                return function () { _switchTab(btn.dataset.tab); };
            })(tabBtns[i]));
        }

 // ── 初始目录项（外部跳转通道 / 深链 / 会话记忆 / 默认项）──
        var entry = _resolveInitialEntry();
        _selectEntry(entry.tab, entry.key, {});
    }

    /**
     * 构建一个页签的左右布局（目录栏 + 内容区）。
     *
     * 结构:
     * - `.settings-layout` = `.settings-nav`（目录）+ `.settings-content`（面包屑 /
     *   跨页签链接 / 高级固定区 / 目录项内容容器）。
     */
    function _buildPaneLayout(pane, tab) {
        var layout = document.createElement('div');
        layout.className = 'settings-layout';

        var nav = document.createElement('div');
        nav.className = 'settings-nav';
        nav.id = tab === 'basic' ? 'settings-basic-nav' : 'settings-adv-nav';
        nav.innerHTML = _navHtml(tab);
        nav.addEventListener('click', function (ev) {
            // 沿父链定位目录项按钮（不依赖 closest，兼容性更稳）
            var target = ev.target;
            while (target && target !== nav) {
                if (target.classList && target.classList.contains('settings-nav-item')) {
                    _selectEntry(target.getAttribute('data-tab'), target.getAttribute('data-key'));
                    return;
                }
                target = target.parentNode;
            }
        });

        var content = document.createElement('div');
        content.className = 'settings-content';

        var breadcrumb = document.createElement('div');
        breadcrumb.className = 'settings-breadcrumb';
        breadcrumb.id = tab === 'basic' ? 'settings-basic-breadcrumb' : 'settings-adv-breadcrumb';
        content.appendChild(breadcrumb);

        if (tab === 'advanced') {
            var crosslink = document.createElement('div');
            crosslink.id = 'settings-adv-crosslink';
            content.appendChild(crosslink);

            // 固定区：风险提示条 + 全局恢复默认栏（不随目录项切换消失）
            var headerZone = document.createElement('div');
            headerZone.id = 'settings-adv-header-zone';
            NS.parts.advanced.renderHeader(headerZone);
            content.appendChild(headerZone);
        }

        var body = document.createElement('div');
        body.className = 'settings-body';
        body.id = tab === 'basic' ? 'settings-basic-body' : 'settings-adv-body';
        content.appendChild(body);

        layout.appendChild(nav);
        layout.appendChild(content);
        pane.appendChild(layout);
    }

    /**
     * 生成目录栏 HTML（一级分组标题 + 目录项按钮）。
     *
     * 说明: 高级目录项标题取自分组元数据（与表单标题同源）；基础项标题来自目录表。
     */
    function _navHtml(tab) {
        var html = '';
        if (tab === 'basic') {
            for (var i = 0; i < NS.NAV_BASIC.length; i++) {
                var group = NS.NAV_BASIC[i];
                html += '<div class="settings-nav-group">' +
                    '<div class="settings-nav-group-title">' + group.title + '</div>';
                for (var j = 0; j < group.items.length; j++) {
                    html += '<button type="button" class="settings-nav-item" data-tab="basic" data-key="' +
                        group.items[j].key + '">' + group.items[j].title + '</button>';
                }
                html += '</div>';
            }
            return html;
        }

        var meta = _advancedGroupMeta();
        for (var k = 0; k < NS.NAV_ADVANCED.length; k++) {
            var advGroup = NS.NAV_ADVANCED[k];
            html += '<div class="settings-nav-group">' +
                '<div class="settings-nav-group-title">' + advGroup.title + '</div>';
            for (var m = 0; m < advGroup.keys.length; m++) {
                var key = advGroup.keys[m];
                html += '<button type="button" class="settings-nav-item" data-tab="advanced" data-key="' +
                    key + '">' + (meta[key] ? meta[key].title : key) + '</button>';
            }
            html += '</div>';
        }
        return html;
    }

    /**
     * 切换基础/高级 Tab 并定位到该页签上次选中的目录项。
     *
     * 说明: 已渲染同项时不重渲染（保留未保存的输入），仅刷新页签显隐与导航高亮。
     */
    function _switchTab(tab) {
        if (tab === 'advanced' && !state.debugEnabled) return;
        _applyTabVisibility(tab);
        var entry = NS.resolveNavEntry(tab, state.lastEntry[tab]) ||
            NS.resolveNavEntry(tab, tab === 'basic' ? 'backend' : 'retrieval');
        if (!entry) return;
        _selectEntry(entry.tab, entry.key, { skipTabSwitch: true, keepRendered: true });
    }

    /**
     * 应用页签显隐（按钮 active + pane 显隐）。
     */
    function _applyTabVisibility(tab) {
        var btns = document.querySelectorAll('.settings-tab-btn');
        for (var i = 0; i < btns.length; i++) {
            btns[i].classList.toggle('active', btns[i].dataset.tab === tab);
        }
        var basic = NS.$('settings-pane-basic');
        var advanced = NS.$('settings-pane-advanced');
        if (basic) basic.classList.toggle('hidden', tab !== 'basic');
        if (advanced) advanced.classList.toggle('hidden', tab !== 'advanced');
    }

    /**
     * 选中目录项：页签定位 / 导航高亮 / 面包屑 / 跨页签链接 / 内容渲染与回显 / 深链同步。
     *
     * 参数:
     * - `tab`: `basic` | `advanced`（高级不可见时静默忽略）。
     * - `key`: 目录项 key（未知 key 静默忽略，等价降级到不动作）。
     * - `opts.skipTabSwitch`: 跳过页签显隐应用（页签切换流程内部调用）。
     * - `opts.keepRendered`: 已渲染同项时不重渲染（保留未保存的输入）。
     */
    function _selectEntry(tab, key, opts) {
        opts = opts || {};
        if (tab === 'advanced' && !state.debugEnabled) return;
        var entry = NS.resolveNavEntry(tab, key);
        if (!entry) return;
        if (!opts.skipTabSwitch) _applyTabVisibility(entry.tab);

        state.currentEntry = entry;
        state.lastEntry[entry.tab] = entry.key;

        _refreshNavActive(entry.tab, entry.key);
        _renderBreadcrumb(entry);
        _renderCrossLink(entry);

        if (!(opts.keepRendered && _isRendered(entry.tab, entry.key))) {
            _renderActiveSection();
            _refreshActiveData(entry);
        }
        _syncHash(entry.tab, entry.key);
        _scrollTop();
    }

    /**
     * 目录栏当前项高亮（active + aria-current）。
     */
    function _refreshNavActive(tab, key) {
        var nav = NS.$(tab === 'basic' ? 'settings-basic-nav' : 'settings-adv-nav');
        if (!nav) return;
        var items = nav.querySelectorAll('.settings-nav-item');
        for (var i = 0; i < items.length; i++) {
            var active = items[i].getAttribute('data-key') === key;
            items[i].classList.toggle('active', active);
            if (active) {
                items[i].setAttribute('aria-current', 'true');
            } else {
                items[i].removeAttribute('aria-current');
            }
        }
    }

    /**
     * 当前内容容器是否已渲染指定目录项。
     */
    function _isRendered(tab, key) {
        var body = NS.$(tab === 'basic' ? 'settings-basic-body' : 'settings-adv-body');
        return !!(body && body.getAttribute('data-entry-key') === key);
    }

    /**
     * 刷新内容区面包屑（`基础设置 / 分组 / 项标题`；分组名可点击跳该分组首项）。
     */
    function _renderBreadcrumb(entry) {
        var bc = NS.$(entry.tab === 'basic' ? 'settings-basic-breadcrumb' : 'settings-adv-breadcrumb');
        if (!bc) return;

        var title = entry.title;
        if (!title) {
            var g = _advancedGroupMeta()[entry.key];
            title = g ? g.title : entry.key;
        }
        var firstKey = NS.firstKeyOfGroup(entry.tab, entry.group);

        var html = '<span>' + (entry.tab === 'basic' ? '基础设置' : '高级设置') + '</span>';
        if (entry.group && firstKey) {
            html += ' / <button type="button" class="settings-breadcrumb-link" data-tab="' + entry.tab +
                '" data-key="' + firstKey + '">' + entry.group + '</button>';
        } else if (entry.group) {
            html += ' / <span>' + entry.group + '</span>';
        }
        html += ' / <span>' + title + '</span>';
        bc.innerHTML = html;

        var link = bc.querySelector('.settings-breadcrumb-link');
        if (link) {
            link.addEventListener('click', function () {
                _selectEntry(link.getAttribute('data-tab'), link.getAttribute('data-key'));
            });
        }
    }

    /**
     * 刷新高级项目的"对应基础项"反向跳转行（无映射或高级不可见时为空白）。
     */
    function _renderCrossLink(entry) {
        var box = NS.$('settings-adv-crosslink');
        if (!box) return;
        var targets = entry.tab === 'advanced' ? NS.CROSSLINKS.advancedToBasic[entry.key] : null;
        if (!targets || !targets.length) {
            box.innerHTML = '';
            return;
        }

        var titles = {};
        for (var i = 0; i < NS.NAV_BASIC.length; i++) {
            for (var j = 0; j < NS.NAV_BASIC[i].items.length; j++) {
                titles[NS.NAV_BASIC[i].items[j].key] = NS.NAV_BASIC[i].items[j].title;
            }
        }

        var links = '';
        for (var k = 0; k < targets.length; k++) {
            if (!titles[targets[k]]) continue;
            if (links) links += ' · ';
            links += '<button type="button" class="settings-breadcrumb-link" data-tab="basic" data-key="' +
                targets[k] + '">' + titles[targets[k]] + '</button>';
        }
        if (!links) {
            box.innerHTML = '';
            return;
        }

        box.innerHTML = '<div class="settings-crosslink"><span>对应基础项 ←</span>' + links + '</div>';
        var btns = box.querySelectorAll('button');
        for (var m = 0; m < btns.length; m++) {
            btns[m].addEventListener('click', (function (btn) {
                return function () {
                    _selectEntry(btn.getAttribute('data-tab'), btn.getAttribute('data-key'));
                };
            })(btns[m]));
        }
    }

    /**
     * 渲染当前选中目录项：清空内容容器 → 渲染 → 同步回显缓存数据。
     */
    function _renderActiveSection() {
        var entry = state.currentEntry;
        if (!entry) return;
        var isBasic = entry.tab === 'basic';
        var body = NS.$(isBasic ? 'settings-basic-body' : 'settings-adv-body');
        if (!body) return;

        body.innerHTML = '';
        body.setAttribute('data-entry-key', entry.key);

        var parts = NS.parts;
        if (isBasic) {
            switch (entry.key) {
                case 'backend': parts.backend.render(body); break;
                case 'embedding': parts.embedding.render(body); break;
                case 'session': parts.session.render(body); break;
                case 'proactive': parts.proactive.render(body); break;
                case 'channel': parts.channel.render(body); break;
                case 'privacy': parts.privacy.render(body); break;
                case 'data_dir': parts.dataDir.render(body); break;
                case 'mcp': parts.mcp.render(body); break;
                case 'data': parts.data.render(body); break;
                case 'diagnostics': parts.diagnostics.render(body); break;
                case 'about': parts.about.render(body); break;
                default: break;
            }
            _fillActiveSection();
            _appendBasicCrosslink(body, entry.key);
            return;
        }

        var group = _advancedGroupMeta()[entry.key];
        if (group) {
            parts.advanced.renderGroup(body, group);
            if (state.fullConfig) parts.advanced.fillGroup(group, state.fullConfig);
        }
    }

    /**
     * 按当前选中目录项回显缓存数据（同步；数据未到达时保持表单默认值）。
     */
    function _fillActiveSection() {
        var entry = state.currentEntry;
        if (!entry) return;
        if (entry.tab === 'advanced') {
            var group = _advancedGroupMeta()[entry.key];
            if (group && state.fullConfig) NS.parts.advanced.fillGroup(group, state.fullConfig);
            return;
        }

        var parts = NS.parts;
        switch (entry.key) {
            case 'backend':
                parts.backend.fill(state.backendConfig);
                parts.backend.fillVision(state.fullConfig);
                break;
            case 'embedding':
                parts.embedding.fill(state.embeddingConfig);
                break;
            case 'session':
                parts.session.fill(state.fullConfig);
                break;
            case 'proactive':
                parts.proactive.fill(state.fullConfig);
                break;
            case 'data_dir':
                parts.dataDir.fill(state.fullConfig);
                break;
            case 'mcp':
                parts.mcp.fill(state.fullConfig);
                break;
            default:
                // channel / privacy / data / diagnostics / about：数据由各自异步加载或纯静态
                break;
        }
    }

    /**
     * 触发当前目录项的异步数据刷新（名单 / 通道面板 / MCP 信息 / 隐私状态）。
     *
     * 说明: 异步失败由各加载函数内部降级处理，不阻塞渲染。
     */
    function _refreshActiveData(entry) {
        if (entry.tab !== 'basic') return;
        switch (entry.key) {
            case 'proactive': NS.parts.proactive.loadPersonas(); break;
            case 'channel': NS.parts.channel.loadPanel(); break;
            case 'mcp': NS.parts.mcp.refreshInfo(); break;
            case 'privacy': NS.parts.privacy.refreshStatus(); break;
            default: break;
        }
    }

    /**
     * 基础项底部渲染"高级设置 →"跳转行（高级不可见或无映射时不渲染）。
     */
    function _appendBasicCrosslink(body, key) {
        if (!state.debugEnabled) return;
        var targets = NS.CROSSLINKS.basicToAdvanced[key];
        if (!targets || !targets.length) return;

        var meta = _advancedGroupMeta();
        var links = '';
        for (var i = 0; i < targets.length; i++) {
            if (!meta[targets[i]]) continue;
            if (links) links += ' · ';
            links += '<button type="button" class="settings-breadcrumb-link" data-tab="advanced" data-key="' +
                targets[i] + '">' + meta[targets[i]].title + '</button>';
        }
        if (!links) return;

        var box = document.createElement('div');
        box.className = 'settings-crosslink';
        box.innerHTML = '<span>高级设置：</span>' + links;
        body.appendChild(box);

        var btns = box.querySelectorAll('button');
        for (var j = 0; j < btns.length; j++) {
            btns[j].addEventListener('click', (function (btn) {
                return function () {
                    _selectEntry(btn.getAttribute('data-tab'), btn.getAttribute('data-key'));
                };
            })(btns[j]));
        }
    }

    /**
     * 高级分组元数据的 key → 组对象映射。
     */
    function _advancedGroupMeta() {
        var map = {};
        var groups = NS.parts.advanced.groups;
        for (var i = 0; i < groups.length; i++) map[groups[i].key] = groups[i];
        return map;
    }

    /**
     * 解析进入设置页时的初始目录项。
     *
     * 优先级:
     * 1. Router options 的 settingsSection（`{tab}/{key}` 外部跳转通道）；
     * 2. location.hash 深链（`#settings/{tab}/{key}`）；
     * 3. 会话内上次选中项（state.lastEntry）；
     * 4. 默认项（基础设置首项）。
     *
     * 非法 / 未知 key 降级到默认项；高级不可见时 advanced 目标退回基础默认。
     */
    function _resolveInitialEntry() {
        var options = {};
        try {
            if (typeof RamariaRouter !== 'undefined' && typeof RamariaRouter.getLastOptions === 'function') {
                options = RamariaRouter.getLastOptions() || {};
            }
        } catch (_) { options = {}; }

        if (options && typeof options.settingsSection === 'string') {
            var segs = options.settingsSection.split('/');
            if (segs.length === 2) {
                var viaOptions = NS.resolveNavEntry(segs[0], segs[1]);
                if (viaOptions) return _visibleEntryOrFallback(viaOptions);
            }
        }

        var hash = '';
        try {
            if (window.location && typeof window.location.hash === 'string') hash = window.location.hash;
        } catch (_) { hash = ''; }
        var parsed = NS.parseSettingsHash(hash);
        if (parsed) {
            var viaHash = NS.resolveNavEntry(parsed.tab, parsed.key);
            if (viaHash) return _visibleEntryOrFallback(viaHash);
        }

        var remembered = NS.resolveNavEntry('basic', state.lastEntry.basic);
        if (remembered) return remembered;
        return NS.resolveNavEntry('basic', 'backend');
    }

    /**
     * 高级不可见时把 advanced 目标退回基础默认项（深链降级约定）。
     */
    function _visibleEntryOrFallback(entry) {
        if (entry.tab === 'advanced' && !state.debugEnabled) {
            return NS.resolveNavEntry('basic', 'backend');
        }
        return entry;
    }

    /**
     * 同步深链 hash（replaceState 不产生 hashchange；环境不支持时静默跳过）。
     */
    function _syncHash(tab, key) {
        try {
            if (window.history && typeof window.history.replaceState === 'function') {
                window.history.replaceState(null, '', '#settings/' + tab + '/' + key);
            }
        } catch (_) { /* ignore */ }
    }

    /**
     * 切换目录项后滚动回设置页顶部。
     */
    function _scrollTop() {
        try {
            var scroll = document.querySelector('.settings-scroll');
            if (scroll) scroll.scrollTop = 0;
        } catch (_) { /* ignore */ }
    }

 // =========================================================
 // 生命周期
 // =========================================================

    function _registerHooks() {
        var unreg;

        unreg = RamariaRouter.registerHook('settings', 'enter', async function () {
            console.log('[SettingsView] 进入视图');
            render();

 // 加载版本信息（静默调用，不显示 toast）
            NS.loadVersion();

 // 加载配置
            try {
                state.backendConfig = await RamariaApi.config.getBackend();
                _fillActiveSection();
                RamariaStore.set('backendConfig', state.backendConfig);
            } catch (err) {
                console.error('[SettingsView] 加载后端配置失败:', err);
            }

 // 加载嵌入模型配置
            try {
                state.embeddingConfig = await RamariaApi.setup.getEmbeddingModel();
                _fillActiveSection();
            } catch (err) {
                console.error('[SettingsView] 加载嵌入模型配置失败:', err);
            }

 // 加载完整配置并回显（会话 / 主动对话 / 记忆注入 / 图片理解 / 数据目录 / MCP / 高级表单）
            try {
                state.fullConfig = await RamariaApi.config.getFull();
                _fillActiveSection();
            } catch (err) {
                console.error('[SettingsView] 加载完整配置失败:', err);
            }

// 加载主动消息名单（逐人格开关；读取失败只显示错误态，不阻塞其余区块）
            await NS.parts.proactive.loadPersonas();

// 加载 MCP 接入信息（运行状态 + 客户端配置片段）
            await NS.parts.mcp.refreshInfo();

// 加载社交平台接入状态（配置摘要 / 凭据标记 / 连接状态 / 绑定列表）
            await NS.parts.channel.loadPanel();
            NS.parts.channel.subscribeStatus();

 // 加载隐私状态
            await NS.parts.privacy.refreshStatus();
        });
        state.unregisterFns.push(unreg);

        unreg = RamariaRouter.registerHook('settings', 'leave', function () {
            console.log('[SettingsView] 离开视图');
            for (var i = 0; i < state.unsubs.length; i++) {
                try { state.unsubs[i](); } catch (_) { /* ignore */ }
            }
            state.unsubs = [];
        });
        state.unregisterFns.push(unreg);
    }

    function init() {
        console.log('[SettingsView] 初始化设置视图...');
        _registerHooks();
    }

 // =========================================================
 // 公开 API
 // =========================================================

    return {
        init: init,
        /**
         * 高级配置组元数据快照（深拷贝，只读）。
         *
         * 用途:
         * - 默认值一致性回归（与 `config/default.toml` 逐键比对，防前端默认值漂移）；
         * - 排障时查看前端认识的配置键集合与默认值。
         *
         * 返回:
         * - 高级分组元数据的深拷贝；调用方修改不会影响设置页内部状态。
         */
        getAdvancedGroups: function () {
            return JSON.parse(JSON.stringify(NS.parts.advanced.groups));
        },
        /**
         * MCP 接入面板字段元数据快照（深拷贝，只读）。
         *
         * 用途:
         * - 默认值一致性回归（与 `config/default.toml` 的 `[mcp]` 组逐键比对）；
         * - 排障时查看面板认识的配置键与默认值。
         *
         * 返回:
         * - MCP 字段元数据的深拷贝；字段 `path` 为组内相对路径（前缀 `mcp`）。
         */
        getMcpFields: function () {
            return JSON.parse(JSON.stringify(NS.parts.mcp.fields));
        },
        /**
         * 图片理解字段元数据快照（深拷贝，只读）。
         *
         * 用途:
         * - 默认值一致性回归（与 `config/default.toml` 的 `[vision]` 组逐键比对）；
         * - 排障时查看面板认识的配置键与默认值。
         *
         * 返回:
         * - 图片理解字段元数据的深拷贝；字段 `path` 为组内相对路径（前缀 `vision`）。
         */
        getVisionFields: function () {
            return JSON.parse(JSON.stringify(NS.parts.backend.visionFields));
        },
        /**
         * 图片理解表单回显（供回归测试直接驱动）。
         */
        fillVisionForm: function (config) {
            return NS.parts.backend.fillVision(config);
        },
        /**
         * 单批上限输入组显隐同步（供回归测试直接驱动）。
         */
        syncVisionBatchVisibility: function () {
            return NS.parts.backend.syncVisionBatchVisibility();
        },
        /**
         * 图片理解表单值收集与校验（纯读取，供回归测试直接驱动）。
         */
        collectVisionValues: function () {
            return NS.parts.backend.collectVisionValues();
        },
        /**
         * 图片理解表单值合并完整配置（纯函数，供回归测试直接驱动）。
         */
        applyVisionFormToConfig: function (config, values) {
            return NS.parts.backend.applyVisionFormToConfig(config, values);
        },
        /**
         * 生成 MCP 挂载配置片段（纯函数，只读）。
         *
         * 参数:
         * - `info`: `{ command, dbPath }`（get_mcp_info 返回值的子集）。
         *
         * 返回:
         * - 片段数组：通用配置（JSON）+ DeepSeek Harness（YAML），结构见片段生成实现。
         */
        buildMcpClientSnippets: function (info) {
            return NS.parts.mcp.buildClientSnippets(info);
        },
        /**
         * 主动消息名单行视图模型（纯函数，只读）。
         *
         * 用途:
         * - 回归测试直接驱动四态文案与 user 行禁用矩阵；
         * - 参数为 list_proactive_personas 的返回数组。
         */
        buildProactivePersonaRows: function (list) {
            return NS.parts.proactive.buildRows(list);
        },
        /**
         * 名单状态文案（纯函数，只读）。
         */
        proactivePersonaStatusText: function (mode, hasDialogue, kind) {
            return NS.parts.proactive.statusText(mode, hasDialogue, kind);
        },
        /**
         * 名单行开关切换处理器（供回归测试直接驱动）。
         *
         * 参数:
         * - `selectEl`: 名单行三态控件（dataset: uid / hasDialogue / kind / prevMode）。
         */
        handleProactivePersonaChange: function (selectEl) {
            return NS.parts.proactive.handlePersonaChange(selectEl);
        },
        /**
         * 加载并渲染主动消息名单（供回归测试直接驱动）。
         */
        loadProactivePersonas: function () {
            return NS.parts.proactive.loadPersonas();
        },
        /**
         * 通道面板字段元数据快照（深拷贝，只读）。
         *
         * 用途:
         * - 默认值一致性回归（与 `config/default.toml` 的 `[channel]` 组逐键比对，
         *   并与高级设置页 `channel` 组元数据交叉锁定）；
         * - 排障时查看面板认识的配置键与默认值。
         */
        getChannelFields: function () {
            return JSON.parse(JSON.stringify(NS.parts.channel.fields));
        },
        /**
         * 社交通道绑定行视图模型（纯函数，只读）。
         */
        buildChannelBindingRows: function (bindings) {
            return NS.parts.channel.buildBindingRows(bindings);
        },
        /**
         * 通道连接状态展示视图（纯函数，只读）。
         */
        channelStateView: function (state) {
            return NS.parts.channel.stateView(state);
        },
        /**
         * 平台凭据保留键（纯函数，只读）。
         */
        channelCredentialKey: function (platform) {
            return NS.parts.channel.credentialKey(platform);
        },
        /**
         * 面板表单值合并进完整配置（纯函数，只读）。
         */
        applyChannelFormToConfig: function (config, values) {
            return NS.parts.channel.applyFormToConfig(config, values);
        },
        /**
         * 保存平台凭据（供回归测试直接驱动；经设置命令保留键写入 keychain）。
         */
        saveChannelCredential: function (platform, value) {
            return NS.parts.channel.saveCredential(platform, value);
        },
        /**
         * 凭据保存处理器（供回归测试直接驱动；空输入仅提示不写入）。
         */
        handleSaveChannelCredential: function (platform) {
            return NS.parts.channel.handleSaveCredential(platform);
        },
        destroy: function () {
            for (var i = 0; i < state.unregisterFns.length; i++) {
                try { state.unregisterFns[i](); } catch (_) { /* ignore */ }
            }
            state.unregisterFns = [];
            for (var j = 0; j < state.unsubs.length; j++) {
                try { state.unsubs[j](); } catch (_) { /* ignore */ }
            }
            state.unsubs = [];
            console.log('[SettingsView] 已销毁');
        },
    };
})();

// 自动初始化
(function _autoInit() {
    if (typeof RamariaRouter === 'undefined') {
        setTimeout(_autoInit, 50);
        return;
    }
    RamariaSettingsView.init();

    var currentView = RamariaRouter.getCurrentView();
    if (currentView === 'settings') {
        setTimeout(function () {
            if (RamariaRouter.getCurrentView() === 'settings') {
                RamariaRouter.showView('settings', { forceReenter: true });
            }
        }, 10);
    }
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaSettingsView', {
    value: RamariaSettingsView,
    writable: false,
    configurable: false,
});
