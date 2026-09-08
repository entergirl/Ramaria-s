/**
 * js/views/debug.js — 调试面板（v2.0 M7）
 *
 * 功能（三个只读/轻操作页签）:
 * 1. 关键词池：三态分层（canonical / alias / pending）+ 使用次数；
 *    待确认别名可 confirm（合并规范词）/ reject（晋升规范词）。
 * 2. 说话风格：按人格展示 persona_style_stats（样本量/状态/规则文本/五维统计）。
 * 3. 评估产物：目录选择 → JSON 产物列表 → 单文件解析摘要（RamariaProbe 纯函数），
 *    展示档位得分与辅助指标；全程只读。
 *
 * 设计特点:
 * - 与 rules.js / memory.js 相同 IIFE + Router 生命周期模式。
 * - 数据经 escapeHtml 消毒；空/加载/错误态齐备。
 * - 评估目录路径记忆于 localStorage（仅调试便利，非敏感）。
 *
 * 依赖: RamariaApi, RamariaRouter, RamariaProbe, RamariaEscape, RamariaToast
 */

var RamariaDebugView = (function () {
    'use strict';

 // =========================================================
 // 内部状态
 // =========================================================

    var EVAL_DIR_KEY = 'ramaria:debug:evalDir';
    var _activeTab = 'keywords';   // 'keywords' | 'style' | 'eval'
    var _personas = [];
    var _styleUid = null;
    var _loading = false;
    var _unregisterHooks = null;
    /** 当前评估目录文件列表缓存（避免重复列出） */
    var _evalFiles = [];
    var _evalDir = null;

 // =========================================================
 // 初始化与生命周期
 // =========================================================

    function init() {
        if (!window.RamariaRouter) return;
        _unregisterHooks = [
            RamariaRouter.registerHook('debug', 'enter', _onEnter),
            RamariaRouter.registerHook('debug', 'leave', _onLeave),
        ];
        console.log('[DebugView] 初始化完成');
    }

    function destroy() {
        if (_unregisterHooks) {
            for (var i = 0; i < _unregisterHooks.length; i++) _unregisterHooks[i]();
            _unregisterHooks = null;
        }
        _personas = [];
        _loading = false;
    }

    function _onEnter() {
        RamariaRouter.setContentTitle('调试面板');
        RamariaRouter.setContentActions('');
        _load();
    }

    function _onLeave() {
        _loading = false;
    }

    function _container() {
        return document.getElementById('view-debug');
    }

 // =========================================================
 // 数据加载与渲染（tab 外壳 + 子页切换）
 // =========================================================

    async function _load() {
        var c = _container();
        if (!c) return;
        c.innerHTML = _buildSkeleton();
        _loading = true;
        try {
            // 预加载人格列表（关键词页不需要，风格页使用）
            try {
                _personas = await RamariaApi.memory.getPersonas();
            } catch (e) {
                _personas = [];
            }
            _loading = false;
            c.innerHTML = '';
            c.appendChild(_buildShell());
            _renderTab();
        } catch (err) {
            _loading = false;
            c.innerHTML = _buildError(err.message || '未知错误');
        }
    }

    function _buildShell() {
        var wrapper = document.createElement('div');
        wrapper.className = 'debug-page';

        var tabs = document.createElement('div');
        tabs.className = 'debug-tabs';
        var defs = [
            { key: 'keywords', icon: '🔑', label: '关键词池', desc: 'keyword_pool 三态与别名处理' },
            { key: 'style', icon: '🎨', label: '说话风格', desc: '五维风格统计（只读）' },
            { key: 'eval', icon: '🧪', label: '评估产物', desc: 'probe 结果只读查看' },
        ];
        var tabHtml = '';
        for (var i = 0; i < defs.length; i++) {
            var d = defs[i];
            tabHtml += '<button class="debug-tab' + (_activeTab === d.key ? ' active' : '') + '" data-tab="' + d.key + '">' +
                '<span class="debug-tab-icon" aria-hidden="true">' + d.icon + '</span>' +
                '<span>' + d.label + '</span></button>';
        }
        tabs.innerHTML = tabHtml;
        wrapper.appendChild(tabs);

        var panel = document.createElement('div');
        panel.className = 'debug-panel';
        panel.id = 'debug-panel';
        wrapper.appendChild(panel);

        // 页签点击
        var tabBtns = tabs.querySelectorAll('.debug-tab');
        for (var j = 0; j < tabBtns.length; j++) {
            tabBtns[j].addEventListener('click', function () {
                _activeTab = this.getAttribute('data-tab');
                var all = tabs.querySelectorAll('.debug-tab');
                for (var k = 0; k < all.length; k++) all[k].classList.remove('active');
                this.classList.add('active');
                _renderTab();
            });
        }
        return wrapper;
    }

    async function _renderTab() {
        var panel = document.getElementById('debug-panel');
        if (!panel) return;
        panel.innerHTML = _tabLoading();
        try {
            if (_activeTab === 'keywords') await _renderKeywords(panel);
            else if (_activeTab === 'style') await _renderStyle(panel);
            else await _renderEval(panel);
        } catch (err) {
            console.error('[DebugView] 渲染页签失败:', err);
            panel.innerHTML = _buildError(err.message || '未知错误');
        }
    }

    function _tabLoading() {
        return '<div class="debug-tab-loading"><span class="spinner-ring" aria-label="加载中"></span> 加载中...</div>';
    }

 // =========================================================
 // 页签一：关键词池
 // =========================================================

    async function _renderKeywords(panel) {
        panel.innerHTML = _buildPanelHead('🔑 关键词池（keyword_pool）',
            '词条三态：canonical（规范词）/ alias（已确认别名）/ pending（待确认别名）。pending 可确认合并到规范词或驳回为独立规范词；日志与学习管线会持续累积词条。');

        // 数据加载
        var [resp, pendingResp] = await Promise.all([
            RamariaApi.keywords.list(),
            RamariaApi.keywords.pendingAliases(),
        ]);

        var wrap = document.createElement('div');
        var counts = [
            { key: 'canonical', label: '规范词', value: resp.canonical_count || 0, cls: 'ok' },
            { key: 'alias', label: '已确认别名', value: resp.alias_count || 0, cls: 'info' },
            { key: 'pending', label: '待确认别名', value: resp.pending_count || 0, cls: 'warn' },
        ];
        var statHtml = '<div class="debug-stat-row">';
        for (var i = 0; i < counts.length; i++) {
            statHtml += '<div class="debug-stat debug-stat--' + counts[i].cls + '">' +
                '<span class="debug-stat-value">' + counts[i].value + '</span>' +
                '<span class="debug-stat-label">' + counts[i].label + '</span></div>';
        }
        statHtml += '</div>';
        wrap.innerHTML = statHtml;

        // 待确认别名操作区
        var pending = pendingResp.aliases || [];
        if (pending.length) {
            wrap.innerHTML += '<div class="debug-section-title">待确认别名冲突（' + pending.length + '）</div>';
            for (var j = 0; j < pending.length; j++) {
                wrap.innerHTML += _pendingAliasRow(pending[j]);
            }
        } else {
            wrap.innerHTML += '<div class="debug-note">暂无待确认别名冲突</div>';
        }

        // 词条分层列表
        wrap.innerHTML += '<div class="debug-section-title">词条分层（' + (resp.total || 0) + '）</div>';

        var groups = [
            { status: 'canonical', title: '规范词（canonical）', items: [] },
            { status: 'alias', title: '已确认别名（alias）', items: [] },
            { status: 'pending', title: '待确认别名（pending）', items: [] },
        ];
        var kws = resp.keywords || [];
        for (var m = 0; m < kws.length; m++) {
            for (var g = 0; g < groups.length; g++) {
                if (kws[m].status === groups[g].status) groups[g].items.push(kws[m]);
            }
        }
        var hasAny = false;
        for (var x = 0; x < groups.length; x++) {
            var grp = groups[x];
            if (!grp.items.length) continue;
            hasAny = true;
            wrap.innerHTML += '<div class="debug-subsection-title">' + grp.title + ' <span class="text-tertiary">(' + grp.items.length + ')</span></div>';
            wrap.innerHTML += _keywordRows(grp.items);
        }
        if (!hasAny && !pending.length) {
            wrap.innerHTML += '<div class="debug-empty">关键词池为空。对话/记忆管线会自然累积词条，也可在 CLI 用 `ramaria keyword seed` 注入种子词。</div>';
        }

        panel.appendChild(wrap);

        // 绑定 pending 行操作
        var rows = panel.querySelectorAll('[data-alias]');
        for (var b = 0; b < rows.length; b++) {
            rows[b].addEventListener('click', function (e) {
                var btn = e.target.closest('[data-act]');
                if (!btn) return;
                var action = btn.getAttribute('data-act');
                var alias = this.getAttribute('data-alias');
                _resolveAlias(alias, action, panel);
            });
        }
    }

    function _pendingAliasRow(item) {
        var alias = RamariaEscape.escapeHtml(item.alias_keyword);
        var canon = RamariaEscape.escapeHtml(item.canonical_keyword);
        return '<div class="debug-alias-row" data-alias="' + RamariaEscape.escapeHtml(item.alias_keyword) + '">' +
            '<span class="debug-alias-text"><span class="t-code">' + alias + '</span>' +
            '<span class="debug-alias-arrow">→</span><span class="t-code">' + canon + '</span></span>' +
            '<span class="debug-alias-actions">' +
                '<button class="btn btn-secondary btn-sm" data-act="confirm">确认合并</button>' +
                '<button class="btn btn-ghost btn-sm" data-act="reject">驳回为规范词</button>' +
            '</span>' +
        '</div>';
    }

    function _keywordRows(items) {
        var html = '';
        for (var i = 0; i < items.length; i++) {
            var it = items[i];
            var arrow = '';
            if (it.canonical_keyword) {
                arrow = ' <span class="debug-kw-arrow">→ ' + RamariaEscape.escapeHtml(it.canonical_keyword) + '</span>';
            }
            html += '<div class="debug-kw-row">' +
                '<span class="t-code debug-kw-word">' + RamariaEscape.escapeHtml(it.keyword) + '</span>' + arrow +
                '<span class="text-tertiary debug-kw-count">使用 ' + it.use_count + ' 次</span>' +
            '</div>';
        }
        return html;
    }

    async function _resolveAlias(alias, action, panel) {
        try {
            await RamariaApi.keywords.resolveAlias(alias, action);
            var label = action === 'confirm' ? '已确认合并' : '已驳回';
            RamariaToast.success('别名处理成功', alias + ' ' + label);
            await _renderTab();
        } catch (err) {
            RamariaToast.error('操作失败', err.message || '未知错误');
        }
    }

 // =========================================================
 // 页签二：说话风格（只读）
 // =========================================================

    /**
     * 选择默认展示的人格：优先全局默认，其次 rama-0001，最后第一个。
     */
    function _pickDefaultStyleUid() {
        var def = RamariaStore.get('defaultPersonaUid');
        if (def && _personas.some(function (p) { return p.uid === def; })) return def;
        var rama = _personas.filter(function (p) { return p.uid === 'rama-0001'; });
        if (rama.length) return rama[0].uid;
        return _personas.length ? _personas[0].uid : null;
    }

    async function _renderStyle(panel) {
        panel.innerHTML = _buildPanelHead('🎨 说话风格统计（只读）',
            '展示 persona_style_stats：样本量 n_p、统计状态、自动规则文本与五维统计参数。' +
            '数据为统计参数，不含原文消息；低于样本阈值时仅标注数据不足，不生成规则。');

        // 人格选择器
        if (!_personas.length) {
            panel.innerHTML += '<div class="debug-note">尚未注册任何人格</div>';
            return;
        }
        if (!_styleUid) {
            _styleUid = _pickDefaultStyleUid();
        }

        var selectHtml = '<label class="rules-toolbar-label" for="debug-style-select">人格</label>' +
            '<select class="select debug-select" id="debug-style-select">';
        for (var i = 0; i < _personas.length; i++) {
            var p = _personas[i];
            selectHtml += '<option value="' + RamariaEscape.escapeHtml(p.uid) + '"' +
                (p.uid === _styleUid ? ' selected' : '') + '>' +
                RamariaEscape.escapeHtml(p.name || p.uid) + '</option>';
        }
        selectHtml += '</select>';

        var tool = document.createElement('div');
        tool.className = 'debug-toolbar';
        tool.innerHTML = selectHtml +
            '<button class="btn btn-ghost btn-sm" id="debug-style-refresh">⟳ 刷新</button>';
        panel.appendChild(tool);

        var box = document.createElement('div');
        box.id = 'debug-style-box';
        box.className = 'debug-style-box';
        panel.appendChild(box);

        var sel = document.getElementById('debug-style-select');
        if (sel) {
            sel.addEventListener('change', function () {
                _styleUid = sel.value;
                _renderStyleBox();
            });
        }
        var refreshBtn = document.getElementById('debug-style-refresh');
        if (refreshBtn) {
            refreshBtn.addEventListener('click', _renderStyleBox);
        }
        await _renderStyleBox();
    }

    async function _renderStyleBox() {
        var box = document.getElementById('debug-style-box');
        if (!box) return;
        box.innerHTML = _tabLoading();
        try {
            var stats = await RamariaApi.style.getStats(_styleUid);
            if (!stats) {
                box.innerHTML = '<div class="debug-empty">该人格尚无风格统计记录（风格统计在 L1 封存时由表达层统计触发，样本不足或未达到学习节奏时无记录）。</div>';
                return;
            }
            box.innerHTML = _renderStyleStats(stats);
        } catch (err) {
            box.innerHTML = _buildError(err.message || '未知错误');
        }
    }

    function _renderStyleStats(stats) {
        // 状态与来源标签（后端已带中文 label，兜底用状态原文）
        var statusBadge = stats.status_label || stats.status || '—';
        var sourceBadge = stats.rule_source_label || stats.rule_source || '—';

        var html =
            '<div class="debug-stat-row">' +
                '<div class="debug-stat debug-stat--ok"><span class="debug-stat-value">' + stats.sample_count + '</span><span class="debug-stat-label">样本量 n_p</span></div>' +
                '<div class="debug-stat ' + (stats.status === 'ready' ? 'debug-stat--ok' : 'debug-stat--warn') + '"><span class="debug-stat-value">' + RamariaEscape.escapeHtml(statusBadge) + '</span><span class="debug-stat-label">统计状态</span></div>' +
                '<div class="debug-stat debug-stat--info"><span class="debug-stat-value">' + RamariaEscape.escapeHtml(sourceBadge) + '</span><span class="debug-stat-label">规则来源</span></div>' +
                '<div class="debug-stat debug-stat--info"><span class="debug-stat-value">' + stats.baseline_version + '</span><span class="debug-stat-label">基线版本</span></div>' +
            '</div>';

        html += '<div class="debug-section-title">自动规则文本</div>';
        if (stats.rule_text) {
            html += '<div class="debug-rule-text">' + RamariaEscape.escapeHtml(stats.rule_text) + '</div>';
        } else {
            html += '<div class="debug-note">未生成规则文本（数据不足 / 无显著项 / 风格通道关闭）</div>';
        }

        // 五维统计参数
        html += '<div class="debug-section-title">五维统计参数（stats_json）</div>';
        var dims = _parseStatsJson(stats.stats_json);
        if (!dims) {
            html += '<div class="debug-note">统计参数不可解析</div>';
        } else {
            var rows = _statsKeyRows(dims);
            html += rows ? '<div class="debug-kv-grid">' + rows + '</div>' : '<div class="debug-note">统计参数为空</div>';
        }
        return html;
    }

    function _parseStatsJson(raw) {
        if (!raw || typeof raw !== 'string') return null;
        try {
            var v = JSON.parse(raw);
            return v && typeof v === 'object' ? v : null;
        } catch (e) {
            return null;
        }
    }

    var STATS_LABELS = {
        total_chars: '总字符',
        total_sentences: '总句数',
        sentence_len_mean: '平均句长',
        sentence_len_p25: '句长 P25',
        sentence_len_p75: '句长 P75',
        slash_count: '斜杠次数',
        comma_count: '逗号次数',
        newline_count: '换行次数',
        exclaim_count: '感叹号',
        question_count: '问号',
        ellipsis_count: '省略号',
        paren_count: '括号',
        tilde_count: '波浪号',
        sentiment_mean: '情绪均值',
        sentiment_std: '情绪标准差',
        sentiment_n: '情绪样本',
        interjection_count: '语气词次数',
        sentiment_word_messages: '情绪词消息数',
    };

    function _statsKeyRows(dims) {
        var html = '';
        var keys = Object.keys(STATS_LABELS);
        for (var i = 0; i < keys.length; i++) {
            var k = keys[i];
            if (dims[k] === undefined || dims[k] === null) continue;
            var val = dims[k];
            var shown;
            if (typeof val === 'number') shown = val.toFixed ? (Number.isInteger(val) ? String(val) : val.toFixed(3)) : String(val);
            else if (Array.isArray(val)) shown = val.slice(0, 3).join('、') + (val.length > 3 ? '…' : '');
            else shown = String(val);
            html += '<div class="debug-kv-item">' +
                '<span class="debug-kv-label">' + STATS_LABELS[k] + '</span>' +
                '<span class="debug-kv-value">' + RamariaEscape.escapeHtml(shown) + '</span></div>';
        }
        // 词频 / 话题词（数组对象形态）单独展示
        ['word_freq', 'topic_freq'].forEach(function (key) {
            var list = dims[key];
            if (Array.isArray(list) && list.length) {
                var tokens = list.slice(0, 8).map(function (pair) {
                    if (Array.isArray(pair)) return pair[0] + '×' + pair[1];
                    if (pair && typeof pair === 'object') return (pair.word || pair.topic || pair[0]) + '×' + (pair.count || pair.freq || pair[1] || '');
                    return String(pair);
                });
                html += '<div class="debug-kv-item debug-kv-item--wide">' +
                    '<span class="debug-kv-label">' + (key === 'word_freq' ? '口癖词' : '话题词') + '</span>' +
                    '<span class="debug-kv-value">' + RamariaEscape.escapeHtml(tokens.join('、')) + '</span></div>';
            }
        });
        return html;
    }

 // =========================================================
 // 页签三：评估产物（只读）
 // =========================================================

    async function _renderEval(panel) {
        panel.innerHTML = _buildPanelHead('🧪 评估产物（probe，只读）',
            '选择 ramaria-cli `probe run/evaluate/report` 产物目录后查看结果。面板仅解析展示，不触发后端运行、不改任何运行时状态。');

        // 恢复上次目录
        try { _evalDir = window.localStorage.getItem(EVAL_DIR_KEY) || null; } catch (e) { _evalDir = null; }
        _evalFiles = [];

        var tool = document.createElement('div');
        tool.className = 'debug-toolbar';
        tool.innerHTML =
            '<button class="btn btn-primary btn-sm" id="debug-eval-pick">📂 选择产物目录</button>' +
            '<button class="btn btn-ghost btn-sm" id="debug-eval-refresh">⟳ 刷新</button>' +
            '<span class="debug-eval-dir" id="debug-eval-dir">' + RamariaEscape.escapeHtml(_evalDir || '未选择目录') + '</span>';
        panel.appendChild(tool);

        var box = document.createElement('div');
        box.id = 'debug-eval-box';
        box.className = 'debug-eval-box';
        panel.appendChild(box);

        var pickBtn = document.getElementById('debug-eval-pick');
        if (pickBtn) pickBtn.addEventListener('click', _pickEvalDir);
        var refreshBtn = document.getElementById('debug-eval-refresh');
        if (refreshBtn) refreshBtn.addEventListener('click', function () { _loadEvalFiles(true); });

        await _loadEvalFiles(false);
    }

    async function _pickEvalDir() {
        try {
            var dir = await RamariaApi.evaluation.pickDir();
            if (!dir) return; // 用户取消
            _evalDir = dir;
            try { window.localStorage.setItem(EVAL_DIR_KEY, dir); } catch (e) { /* ignore */ }
            var dirEl = document.getElementById('debug-eval-dir');
            if (dirEl) dirEl.textContent = dir;
            await _loadEvalFiles(true);
        } catch (err) {
            RamariaToast.error('目录选择失败', err.message || '未知错误');
        }
    }

    async function _loadEvalFiles(force) {
        var box = document.getElementById('debug-eval-box');
        if (!box) return;
        if (!_evalDir) {
            box.innerHTML = '<div class="debug-empty">请先选择评估产物目录（通常为 CLI 运行 `probe run/evaluate/report` 时 `--output` 指向的目录）。</div>';
            return;
        }
        box.innerHTML = _tabLoading();
        try {
            if (force || !_evalFiles.length) {
                var resp = await RamariaApi.evaluation.listFiles(_evalDir);
                _evalFiles = resp.files || [];
            }
            if (!_evalFiles.length) {
                box.innerHTML = '<div class="debug-empty">该目录下未找到 JSON 产物文件。</div>';
                return;
            }
            box.innerHTML = _renderEvalFileList();
            _bindEvalFileList();
        } catch (err) {
            box.innerHTML = _buildError(err.message || '未知错误');
        }
    }

    function _renderEvalFileList() {
        var html = '<div class="debug-section-title">产物文件（' + _evalFiles.length + '，按修改时间倒序）</div>' +
            '<div class="debug-file-list">';
        for (var i = 0; i < _evalFiles.length; i++) {
            var f = _evalFiles[i];
            html += '<div class="debug-file-row" data-path="' + RamariaEscape.escapeHtml(f.path) + '">' +
                '<span class="debug-file-name">' + RamariaEscape.escapeHtml(f.name) + '</span>' +
                '<span class="debug-file-size text-tertiary">' + (f.size || 0) + ' B</span>' +
                '<span class="debug-file-open">查看 →</span>' +
            '</div>';
        }
        html += '</div>';
        return html;
    }

    function _bindEvalFileList() {
        var rows = document.querySelectorAll('#debug-eval-box .debug-file-row');
        for (var i = 0; i < rows.length; i++) {
            rows[i].addEventListener('click', function () {
                var path = this.getAttribute('data-path');
                _openEvalFile(path);
            });
        }
    }

    async function _openEvalFile(path) {
        var box = document.getElementById('debug-eval-box');
        if (!box) return;
        box.innerHTML = _tabLoading();
        try {
            var value = await RamariaApi.evaluation.readResult(path);
            var summary = RamariaProbe.summarize(value);
            box.innerHTML = _renderEvalSummary(path, summary);
        } catch (err) {
            box.innerHTML = _buildError(err.message || '未知错误');
        }
    }

    function _renderEvalSummary(path, s) {
        var name = path.split(/[\\/]/).pop() || path;
        var kindLabel = { report: '消融报告', evaluate: '评估结果', run: '运行结果', unknown: '未知形态' }[s.kind] || s.kind;
        var html =
            '<div class="debug-file-back-row"><button class="btn btn-ghost btn-sm" id="debug-eval-back">← 返回文件列表</button></div>' +
            '<div class="debug-section-title">' + RamariaEscape.escapeHtml(name) + '</div>';

        var meta = s.meta || {};
        html += '<div class="debug-stat-row">' +
            _statMini(kindLabel, '产物类型') +
            _statMini(meta.personaUid || '—', 'persona') +
            _statMini(meta.judgeUsed === null ? '—' : (meta.judgeUsed ? '已用' : '未用'), '本地 judge') +
            _statMini(s.variants.length, '档位数') +
        '</div>';

        if (!s.hasContent) {
            html += '<div class="debug-note">无法从该文件识别出评估结构（可能是其它 JSON）。</div>';
            return html;
        }

        // 辅助指标（report）
        if (s.auxiliary && s.auxiliary.length) {
            html += '<div class="debug-section-title">辅助指标（report）</div>';
            for (var a = 0; a < s.auxiliary.length; a++) {
                html += '<div class="debug-aux-item"><span>' + s.auxiliary[a].label + '</span>' +
                    '<span class="t-code">' + (s.auxiliary[a].value * 100).toFixed(1) + '%</span></div>';
            }
        }

        // 档位明细
        html += '<div class="debug-section-title">档位得分</div>';
        for (var v = 0; v < s.variants.length; v++) {
            var row = s.variants[v];
            html += '<div class="debug-variant">' +
                '<div class="debug-variant-head"><span class="t-code">' + RamariaEscape.escapeHtml(row.name) + '</span>' +
                (row.ablation ? '<span class="badge badge-gray">' + RamariaEscape.escapeHtml(row.ablation) + '</span>' : '') +
                '</div>' +
                (row.gateText ? '<div class="debug-variant-gate">' + RamariaEscape.escapeHtml(row.gateText) + '</div>' : '');
            if (row.scores && row.scores.length) {
                html += '<div class="debug-variant-scores">';
                for (var q = 0; q < row.scores.length; q++) {
                    var sc = row.scores[q];
                    html += '<span class="debug-score-chip"><b>' + RamariaEscape.escapeHtml(sc.label) + '</b> ' + sc.value +
                        (sc.detail ? ' <em>' + RamariaEscape.escapeHtml(sc.detail) + '</em>' : '') + '</span>';
                }
                html += '</div>';
            } else {
                html += '<div class="text-tertiary t-small">该档位无标量得分</div>';
            }
            html += '</div>';
        }
        return html;
    }

    function _statMini(value, label) {
        return '<div class="debug-stat debug-stat--mini"><span class="debug-stat-value">' + RamariaEscape.escapeHtml(String(value === null || value === undefined ? '—' : value)) + '</span><span class="debug-stat-label">' + label + '</span></div>';
    }

 // =========================================================
 // 通用头部 / 状态
 // =========================================================

    function _buildPanelHead(title, desc) {
        return '<div class="debug-panel-head">' +
            '<h3 class="debug-panel-title">' + title + '</h3>' +
            '<p class="debug-panel-desc">' + RamariaEscape.escapeHtml(desc) + '</p>' +
        '</div>';
    }

    function _buildError(message) {
        return '<div class="debug-error">' +
            '<div class="debug-error-icon" aria-hidden="true">⚠️</div>' +
            '<h3 class="debug-error-title">加载失败</h3>' +
            '<p class="debug-error-desc">' + RamariaEscape.escapeHtml(message) + '</p>' +
        '</div>';
    }

    function _buildSkeleton() {
        return '<div class="debug-page">' +
            '<div class="skeleton-line w-60 mb-2"></div>' +
            '<div class="skeleton-line w-90 mb-2"></div>' +
            '<div class="skeleton-line w-80"></div>' +
        '</div>';
    }

 // =========================================================
 // 公开 API
 // =========================================================

    return {
        init: init,
        destroy: destroy,
        reload: _load,
    };
})();

Object.defineProperty(window, 'RamariaDebugView', {
    value: RamariaDebugView,
    writable: false,
    configurable: false,
});

// =========================================================
// 自动初始化
// =========================================================

(function autoInit() {
    if (window.RamariaRouter) {
        RamariaDebugView.init();
    } else {
        var attempts = 0;
        var interval = setInterval(function () {
            attempts++;
            if (window.RamariaRouter) {
                clearInterval(interval);
                RamariaDebugView.init();
            } else if (attempts >= 50) {
                clearInterval(interval);
                console.error('[DebugView] 等待 RamariaRouter 超时');
            }
        }, 200);
    }
})();
