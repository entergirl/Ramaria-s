/**
 * js/views/rules.js — 行为规则管理视图（v2.0 M7）
 *
 * 功能:
 * - 按 persona 列出行为规则（启用/禁用、自动/手工来源、置信度/稳定性）
 * - 规则详情（含证据链只读视图）
 * - 启用 / 禁用规则
 * - 手工编辑规则（reaction / avoid；编辑后转 Manual 强锚点，与 CLI rule edit 一致）
 * - **不提供删除路径**（回归红线 7，事实/规则只增不删入口）
 *
 * 设计特点:
 * - 对接 T-V20-3 起既有后端：list_rules / get_rule / set_rule_enabled / edit_rule /
 *   rule_evidence 均为 CLI `ramaria rule` 同源命令。
 * - 数据含结构化字段；所有动态文本先 escapeHtml。
 * - 全部交互经 RamariaModal / RamariaToast；空/加载/错误态齐备。
 *
 * 依赖: RamariaApi, RamariaRouter, RamariaModal, RamariaToast, RamariaEscape
 */

var RamariaRulesView = (function () {
    'use strict';

 // =========================================================
 // 内部状态
 // =========================================================

    var _personas = [];
    var _personaUid = null;
    var _rules = [];
    var _loading = false;
    var _unregisterHooks = null;

 // =========================================================
 // 初始化与生命周期
 // =========================================================

    function init() {
        if (!window.RamariaRouter) return;
        _unregisterHooks = [
            RamariaRouter.registerHook('rules', 'enter', _onEnter),
            RamariaRouter.registerHook('rules', 'leave', _onLeave),
        ];
        console.log('[RulesView] 初始化完成');
    }

    function destroy() {
        if (_unregisterHooks) {
            for (var i = 0; i < _unregisterHooks.length; i++) _unregisterHooks[i]();
            _unregisterHooks = null;
        }
        _personas = [];
        _rules = [];
        _loading = false;
    }

    function _onEnter() {
        RamariaRouter.setContentTitle('规则管理');
        RamariaRouter.setContentActions('');
        _load();
    }

    function _onLeave() {
        _loading = false;
    }

 // =========================================================
 // 数据加载与渲染
 // =========================================================

    function _container() {
        return document.getElementById('view-rules');
    }

    async function _load() {
        if (_loading) return;
        _loading = true;
        var c = _container();
        if (!c) { _loading = false; return; }
        c.innerHTML = _buildSkeleton();

        try {
            // 1. 人格列表（用于选择器）
            _personas = await RamariaApi.memory.getPersonas();
            if (_personas.length === 0) {
                throw new Error('尚未注册任何人格，无法管理行为规则');
            }
            if (!_personaUid) {
                _personaUid = _pickDefaultUid();
            }
            // 2. 规则列表
            await _refreshRules();
        } catch (err) {
            console.error('[RulesView] 加载失败:', err);
            c.innerHTML = _buildError(err.message || '未知错误');
        } finally {
            _loading = false;
        }
    }

    async function _refreshRules() {
        var c = _container();
        if (!c) return;
        var resp = await RamariaApi.rules.list(_personaUid);
        _rules = resp.rules || [];
        c.innerHTML = '';
        c.appendChild(_buildPage());
    }

    function _buildPage() {
        var wrapper = document.createElement('div');
        wrapper.className = 'rules-page';

        // 工具栏：人格选择 + 计数
        var toolbar = document.createElement('div');
        toolbar.className = 'rules-toolbar';
        toolbar.innerHTML =
            '<label class="rules-toolbar-label" for="rules-persona-select">人格</label>' +
            '<select class="select rules-select" id="rules-persona-select">' +
                _personaOptions() +
            '</select>' +
            '<span class="rules-count" id="rules-count"></span>' +
            '<button class="btn btn-ghost btn-sm" id="rules-refresh-btn">⟳ 刷新</button>';
        wrapper.appendChild(toolbar);

        // 列表或空态
        var listWrap = document.createElement('div');
        listWrap.className = 'rules-list';
        if (_rules.length === 0) {
            listWrap.appendChild(_buildEmpty());
        } else {
            for (var i = 0; i < _rules.length; i++) {
                listWrap.appendChild(_buildRuleCard(_rules[i]));
            }
        }
        wrapper.appendChild(listWrap);

        // 绑定事件（渲染后统一绑定，避免重复）
        setTimeout(function () {
            var sel = document.getElementById('rules-persona-select');
            if (sel) {
                sel.addEventListener('change', function () {
                    _personaUid = sel.value;
                    _renderWithLoading();
                });
            }
            var refreshBtn = document.getElementById('rules-refresh-btn');
            if (refreshBtn) {
                refreshBtn.addEventListener('click', function () {
                    _renderWithLoading();
                });
            }
            var countEl = document.getElementById('rules-count');
            if (countEl) countEl.textContent = '共 ' + _rules.length + ' 条';
        }, 0);

        return wrapper;
    }

    /**
     * 选择默认展示的人格：优先全局默认，其次 rama-0001，最后第一个。
     */
    function _pickDefaultUid() {
        var def = RamariaStore.get('defaultPersonaUid');
        if (def && _personas.some(function (p) { return p.uid === def; })) return def;
        var rama = _personas.filter(function (p) { return p.uid === 'rama-0001'; });
        if (rama.length) return rama[0].uid;
        return _personas.length ? _personas[0].uid : null;
    }

    function _personaOptions() {
        var html = '';
        for (var i = 0; i < _personas.length; i++) {
            var p = _personas[i];
            var name = p.name || p.uid;
            html += '<option value="' + RamariaEscape.escapeHtml(p.uid) + '"' +
                (p.uid === _personaUid ? ' selected' : '') + '>' +
                RamariaEscape.escapeHtml(name) + '</option>';
        }
        return html;
    }

    async function _renderWithLoading() {
        var c = _container();
        if (c) c.innerHTML = _buildSkeleton();
        try {
            await _refreshRules();
        } catch (err) {
            console.error('[RulesView] 刷新失败:', err);
            _toast('加载失败: ' + (err.message || '未知错误'), 'error');
        }
    }

    function _buildRuleCard(rule) {
        var card = document.createElement('div');
        card.className = 'rules-card';
        card.setAttribute('role', 'button');
        card.setAttribute('tabindex', '0');
        card.setAttribute('aria-label', '查看规则 #' + rule.id);

        var enabled = !!rule.enabled;
        var source = rule.source === 'manual' ? '手工' : '自动';
        var sourceCls = rule.source === 'manual' ? 'rules-badge--manual' : 'rules-badge--auto';
        var statusCls = enabled ? 'rules-badge--on' : 'rules-badge--off';

        var reaction = rule.reaction
            ? RamariaEscape.escapeHtml(rule.reaction)
            : '<span class="rules-card-reaction-candidate">（候选规则，仅参数注入）</span>';

        // 情境关键词 chips
        var kw = '';
        var situation = rule.situation || {};
        var keywords = situation.keywords || [];
        if (keywords.length) {
            var chips = [];
            for (var i = 0; i < keywords.length; i++) {
                chips.push('<span class="chip rules-chip">' + RamariaEscape.escapeHtml(keywords[i]) + '</span>');
            }
            kw = '<div class="rules-card-kws">' + chips.join('') + '</div>';
        }

        // 参数摘要
        var params = rule.params || {};
        var pEI = typeof params.emotional_intensity === 'number' ? params.emotional_intensity.toFixed(2) : '—';

        card.innerHTML =
            '<div class="rules-card-head">' +
                '<span class="rules-card-id">#' + rule.id + '</span>' +
                '<span class="rules-badge ' + sourceCls + '">' + source + '</span>' +
                '<span class="rules-badge ' + statusCls + '">' + (enabled ? '启用' : '禁用') + '</span>' +
            '</div>' +
            '<div class="rules-card-body">' +
                '<div class="rules-card-reaction">' + reaction + '</div>' +
                kw +
                '<div class="rules-card-meta">' +
                    '<span>置信度 ' + (typeof rule.confidence === 'number' ? rule.confidence.toFixed(2) : '—') + '</span>' +
                    '<span>稳定性 ' + (typeof rule.stability === 'number' ? rule.stability.toFixed(2) : '—') + '</span>' +
                    '<span>情感强度 ' + pEI + '</span>' +
                '</div>' +
            '</div>' +
            '<div class="rules-card-actions">' +
                '<button class="btn btn-ghost btn-sm rules-act" data-act="detail">详情 / 证据</button>' +
                '<button class="btn btn-ghost btn-sm rules-act" data-act="edit">编辑</button>' +
                '<button class="btn btn-sm ' + (enabled ? 'btn-secondary' : 'btn-primary') + ' rules-act" data-act="toggle">' +
                    (enabled ? '禁用' : '启用') + '</button>' +
            '</div>';

        // 行为按钮委托
        var acts = card.querySelectorAll('.rules-act');
        for (var i = 0; i < acts.length; i++) {
            acts[i].addEventListener('click', function (e) {
                e.stopPropagation();
                var act = this.getAttribute('data-act');
                if (act === 'detail') _openDetail(rule);
                else if (act === 'edit') _openEdit(rule);
                else if (act === 'toggle') _toggleEnabled(rule);
            });
        }

        // 卡片主体点击 → 详情
        card.addEventListener('click', function () { _openDetail(rule); });
        card.addEventListener('keydown', function (e) {
            if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                _openDetail(rule);
            }
        });

        return card;
    }

 // =========================================================
 // 详情 + 证据链
 // =========================================================

    function _openDetail(rule) {
        var body = document.createElement('div');
        body.className = 'rules-detail-body';

        var kw = (rule.situation && rule.situation.keywords || []).join('、') || '—';
        var avoid = rule.avoid && rule.avoid.length ? rule.avoid.join('、') : '（无）';
        var reaction = rule.reaction ? rule.reaction : '（候选规则，仅参数注入）';
        var source = rule.source === 'manual' ? '手工（Manual）' : '自动学习（Auto）';

        body.innerHTML =
            '<div class="rules-detail-grid">' +
                _kv('状态', rule.enabled ? '启用' : '禁用') +
                _kv('来源', source) +
                _kv('规则文本', reaction) +
                _kv('情境关键词', kw) +
                _kv('禁忌列表', avoid) +
                _kv('置信度', rule.confidence !== undefined ? rule.confidence.toFixed(3) : '—') +
                _kv('稳定性', rule.stability !== undefined ? rule.stability.toFixed(3) : '—') +
                _kv('证据数', rule.evidence ? rule.evidence.length : 0) +
            '</div>' +
            '<div class="rules-detail-evidence-title">证据链（规则 → 事件，只读）</div>' +
            '<div class="rules-detail-evidence" id="rules-evidence-box">' +
                '<div class="rules-loading-text">加载证据中...</div>' +
            '</div>';

        RamariaModal.show({
            title: '规则 #' + rule.id + ' 详情',
            body: body,
            size: 'lg',
            footer:
                '<button class="btn btn-secondary btn-sm" data-action="toggle">' + (rule.enabled ? '禁用' : '启用') + '</button>' +
                '<button class="btn btn-primary btn-sm" data-action="edit">编辑</button>',
            onAction: function (action) {
                if (action === 'toggle') {
                    RamariaModal.close();
                    setTimeout(function () { _toggleEnabled(rule); }, 0);
                } else if (action === 'edit') {
                    RamariaModal.close();
                    setTimeout(function () { _openEdit(rule); }, 0);
                }
            },
        });

        // 异步加载证据链
        _loadEvidence(rule.id);
    }

    async function _loadEvidence(ruleId) {
        try {
            var resp = await RamariaApi.rules.evidence(ruleId);
            var box = document.getElementById('rules-evidence-box');
            if (!box) return;
            var items = resp.evidence || [];
            if (items.length === 0) {
                box.innerHTML = '<div class="rules-evidence-empty">暂无证据（手工导入的规则通常无事件证据链）</div>';
                return;
            }
            var html = '';
            for (var i = 0; i < items.length; i++) {
                var it = items[i];
                html +=
                    '<div class="rules-evidence-item">' +
                        '<div class="rules-evidence-head">事件 #' + it.event_id + ' <span class="text-tertiary">权重 ' + (it.weight !== undefined ? it.weight.toFixed(2) : '—') + '</span></div>' +
                        '<div class="rules-evidence-title-text">' + RamariaEscape.escapeHtml(it.title || '') + '</div>' +
                        '<div class="rules-evidence-summary">' + RamariaEscape.escapeHtml(it.summary || '') + '</div>' +
                        (it.paraphrase ? '<div class="rules-evidence-para">态度（脱敏）：' + RamariaEscape.escapeHtml(it.paraphrase) + '</div>' : '') +
                    '</div>';
            }
            box.innerHTML = html;
        } catch (err) {
            var box2 = document.getElementById('rules-evidence-box');
            if (box2) box2.innerHTML = '<div class="rules-evidence-empty">证据加载失败: ' + RamariaEscape.escapeHtml(err.message || String(err)) + '</div>';
        }
    }

    function _kv(label, value) {
        return '<div class="rules-detail-kv">' +
            '<span class="rules-detail-kv-label">' + label + '</span>' +
            '<span class="rules-detail-kv-value">' + RamariaEscape.escapeHtml(String(value)) + '</span>' +
        '</div>';
    }

 // =========================================================
 // 编辑
 // =========================================================

    function _openEdit(rule) {
        var body = document.createElement('div');
        body.innerHTML =
            '<div class="form-group">' +
                '<label class="input-label" for="rules-edit-reaction">规则文本（reaction）</label>' +
                '<textarea class="textarea rules-edit-textarea" id="rules-edit-reaction" rows="3" placeholder="该人格在此情境下应如何回应">' +
                    RamariaEscape.escapeHtml(rule.reaction || '') +
                '</textarea>' +
                '<div class="input-hint">编辑后将转为手工规则（Manual 强锚点）；候选规则若已有文本可补全，否则请留空并仅编辑避免项。</div>' +
            '</div>' +
            '<div class="form-group">' +
                '<label class="input-label" for="rules-edit-avoid">禁忌列表（avoid，逗号分隔）</label>' +
                '<input class="input" id="rules-edit-avoid" type="text" value="' + RamariaEscape.escapeHtml((rule.avoid || []).join(',')) + '" />' +
                '<div class="input-hint">该人格在此情境下应避免的主题词。</div>' +
            '</div>';

        RamariaModal.show({
            title: '编辑规则 #' + rule.id,
            body: body,
            size: 'lg',
            footer:
                '<button class="btn btn-secondary btn-sm" data-action="cancel">取消</button>' +
                '<button class="btn btn-primary btn-sm" data-action="save">保存</button>',
            onAction: function (action) {
                if (action === 'cancel') return; // 自动关闭
                // save：阻止自动关闭，保存成功后再手动关闭；失败保持打开以便修正
                var reactionEl = document.getElementById('rules-edit-reaction');
                var avoidEl = document.getElementById('rules-edit-avoid');
                var reaction = reactionEl ? reactionEl.value : '';
                var avoid = avoidEl ? avoidEl.value : '';
                _saveEdit(rule, reaction, avoid);
                return 'prevent-close';
            },
        });
    }

    async function _saveEdit(rule, reaction, avoid) {
        // 规范化避免列表（兼容中文逗号，后端按英文逗号切分）
        var normalizedAvoid = avoid.replace(/，/g, ',').split(',').map(function (s) { return s.trim(); }).filter(function (s) { return s.length > 0; }).join(',');

        // 无改动检查：避免无意义地把 Auto 转 Manual 并写 S1 反馈
        var hasChange = false;
        var reactionArg = null; // null = 不提交 reaction
        if (reaction.trim()) {
            if (reaction.trim() !== (rule.reaction || '')) hasChange = true;
            reactionArg = reaction.trim();
        } else if (rule.reaction) {
            // 原规则有文本但用户清空：不允许（候选语义由 import 管理）
            _toast('规则文本不能为空（候选规则可保留原状）', 'warning');
            return;
        }
        if (normalizedAvoid !== (rule.avoid || []).join(',')) hasChange = true;

        if (!hasChange) {
            _toast('没有需要保存的修改', 'info');
            return;
        }

        try {
            await RamariaApi.rules.edit(rule.id, reactionArg, normalizedAvoid);
            RamariaModal.close();
            _toast('规则 #' + rule.id + ' 已保存（转为手工）', 'success');
            _renderWithLoading();
        } catch (err) {
            _toast('保存失败: ' + (err.message || '未知错误'), 'error');
        }
    }

 // =========================================================
 // 启用 / 禁用
 // =========================================================

    async function _toggleEnabled(rule) {
        var target = !rule.enabled;
        var label = target ? '启用' : '禁用';
        try {
            await RamariaApi.rules.setEnabled(rule.id, target);
            rule.enabled = target;
            _toast('规则 #' + rule.id + ' 已' + label, 'success');
            _renderWithLoading();
        } catch (err) {
            _toast(label + '失败: ' + (err.message || '未知错误'), 'error');
        }
    }

 // =========================================================
 // 状态页面构建
 // =========================================================

    function _buildEmpty() {
        var div = document.createElement('div');
        div.className = 'rules-empty';
        div.innerHTML =
            '<div class="rules-empty-icon" aria-hidden="true">📋</div>' +
            '<h3 class="rules-empty-title">暂无行为规则</h3>' +
            '<p class="rules-empty-desc">该人格暂无行为规则。行为规则由学习管线在事件聚类后自动生成，' +
                '也可在「调试」面板确认关键词别名后积累更多对话。' +
                '（前端不提供规则删除入口）</p>';
        return div;
    }

    function _buildError(message) {
        var div = document.createElement('div');
        div.className = 'rules-error';
        div.innerHTML =
            '<div class="rules-error-icon" aria-hidden="true">⚠️</div>' +
            '<h3 class="rules-error-title">加载失败</h3>' +
            '<p class="rules-error-desc">' + RamariaEscape.escapeHtml(message) + '</p>' +
            '<button class="btn btn-primary btn-sm" id="rules-error-retry">重试</button>';
        setTimeout(function () {
            var btn = document.getElementById('rules-error-retry');
            if (btn) btn.addEventListener('click', _load);
        }, 0);
        return div;
    }

    function _buildSkeleton() {
        var wrapper = document.createElement('div');
        wrapper.className = 'rules-page';
        var list = document.createElement('div');
        list.className = 'rules-list';
        for (var i = 0; i < 3; i++) {
            var card = document.createElement('div');
            card.className = 'rules-card rules-card--skeleton';
            card.setAttribute('aria-hidden', 'true');
            card.innerHTML =
                '<div class="skeleton-line w-40 mb-2"></div>' +
                '<div class="skeleton-line w-90 mb-2"></div>' +
                '<div class="skeleton-line w-60"></div>';
            list.appendChild(card);
        }
        wrapper.appendChild(list);
        return wrapper.innerHTML;
    }

 // =========================================================
 // UI 辅助
 // =========================================================

    function _toast(message, type) {
        if (!window.RamariaToast || typeof RamariaToast.success !== 'function') {
            console.log('[RulesView Toast] ' + type + ': ' + message);
            return;
        }
        switch (type) {
            case 'success': RamariaToast.success(message); break;
            case 'error': RamariaToast.error(message); break;
            case 'warning': RamariaToast.warning(message); break;
            default: RamariaToast.info(message); break;
        }
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

Object.defineProperty(window, 'RamariaRulesView', {
    value: RamariaRulesView,
    writable: false,
    configurable: false,
});

// =========================================================
// 自动初始化
// =========================================================

(function autoInit() {
    if (window.RamariaRouter) {
        RamariaRulesView.init();
    } else {
        var attempts = 0;
        var interval = setInterval(function () {
            attempts++;
            if (window.RamariaRouter) {
                clearInterval(interval);
                RamariaRulesView.init();
            } else if (attempts >= 50) {
                clearInterval(interval);
                console.error('[RulesView] 等待 RamariaRouter 超时');
            }
        }, 200);
    }
})();
