/**
 * js/views/settings/sections/basic/proactive.js — Ramaria 设置页「主动对话」目录项
 *
 * 职责:
 * - 主动消息四键速览（总开关 / 每日上限 / 最小空闲 / 免打扰时段）
 * - 主动消息名单（逐人格三态开关；行内切换即时保存）
 *
 * 设计特点:
 * - 全局四键经保存按钮提交（update_full_config 双写、后端热生效）；
 *   名单行内切换即时保存，与全局设置形态共存并在文案上区分
 * - 免打扰时段格式校验与高级设置共用 NS.QUIET_HOURS_PATTERN
 *
 * 依赖:
 * - NS.$ / NS.QUIET_HOURS_PATTERN（shared.js）
 * - RamariaApi / RamariaToast / RamariaEscape
 */

(function (NS) {
    'use strict';

    var state = NS.state;

// =========================================================
// 主动对话区块（主动消息调度总开关与打扰控制入口）
// =========================================================

    /**
     * 渲染「主动对话」区块（基础设置页）。
     *
     * 说明:
     * - 四键为日常可达项（总开关 / 每日上限 / 最小空闲 / 免打扰时段）；
     *   检查节拍、判据与选题权重等调度参数在高级设置「主动对话」组。
     */
    function _renderProactiveSection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">🗨️ 主动对话</div>' +
            '<div class="settings-section-desc">系统会在合适时机主动发来消息；免打扰时段内不会打扰。</div>';

        var card = document.createElement('div');
        card.className = 'settings-card';
        card.id = 'settings-proactive-card';
        card.innerHTML =
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">' +
                    '<input type="checkbox" id="settings-proactive-enabled" checked /> 启用主动对话' +
                '</label>' +
                '<div class="settings-form-hint">关闭后不调度、不投递主动消息（总开关）。</div>' +
            '</div>' +
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">每日投递上限（条）</label>' +
                '<input class="settings-form-input" id="settings-proactive-daily-limit" type="number" min="1" max="10" value="3" />' +
                '<div class="settings-form-hint">每个画像每日最多主动投递条数（1~10）。</div>' +
            '</div>' +
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">最小空闲时长（小时）</label>' +
                '<input class="settings-form-input" id="settings-proactive-min-idle" type="number" min="1" max="48" value="4" />' +
                '<div class="settings-form-hint">距上次对话满该时长后才可能主动发起（1~48）。</div>' +
            '</div>' +
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">免打扰时段</label>' +
                '<input class="settings-form-input" id="settings-proactive-quiet-hours" type="text" placeholder="22:00-08:00" />' +
                '<div class="settings-form-hint">HH:MM-HH:MM 格式，支持跨零点（如 22:00-08:00）。</div>' +
            '</div>' +
            '<div class="settings-save-hint">' +
                '<button class="btn btn-primary btn-sm" id="settings-save-proactive">保存主动对话设置</button>' +
            '</div>';

        section.appendChild(card);
        parent.appendChild(section);

        // 主动消息名单子区块（全部活跃人格）：行内切换即时保存，
        // 与上方全局四键的"保存按钮"形态共存并在文案上区分
        var rosterCard = document.createElement('div');
        rosterCard.className = 'settings-card';
        rosterCard.id = 'settings-proactive-roster-card';
        rosterCard.innerHTML =
            '<div class="settings-form-label">主动消息名单</div>' +
            '<div class="settings-form-hint">逐个控制哪些人格可以主动发来消息；行内切换即时保存（上方全局设置需点击保存按钮）。</div>' +
            '<div class="settings-proactive-roster" id="settings-proactive-roster">' +
                '<div class="settings-proactive-roster-placeholder">加载中…</div>' +
            '</div>';
        section.appendChild(rosterCard);

        // 绑定事件
        var saveBtn = NS.$('settings-save-proactive');
        if (saveBtn) saveBtn.addEventListener('click', _handleSaveProactive);
    }

    /**
     * 回显主动对话表单（配置缺失的字段保留 HTML 默认值）。
     */
    function _fillProactiveForm(config) {
        var proactive = config && config.proactive;
        if (!proactive) return;

        var enabledEl = NS.$('settings-proactive-enabled');
        if (enabledEl && typeof proactive.enabled === 'boolean') {
            enabledEl.checked = proactive.enabled;
        }

        var dailyEl = NS.$('settings-proactive-daily-limit');
        if (dailyEl && typeof proactive.daily_limit === 'number') {
            dailyEl.value = proactive.daily_limit;
        }

        var idleEl = NS.$('settings-proactive-min-idle');
        if (idleEl && typeof proactive.min_idle_hours === 'number') {
            idleEl.value = proactive.min_idle_hours;
        }

        var quietEl = NS.$('settings-proactive-quiet-hours');
        if (quietEl && typeof proactive.quiet_hours === 'string') {
            quietEl.value = proactive.quiet_hours;
        }
    }

    /**
     * 保存主动对话设置（读当前值 → 完整配置更新，后端校验并热生效）。
     */
    async function _handleSaveProactive() {
        try {
            if (!state.fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }

            var enabledEl = NS.$('settings-proactive-enabled');
            var dailyEl = NS.$('settings-proactive-daily-limit');
            var idleEl = NS.$('settings-proactive-min-idle');
            var quietEl = NS.$('settings-proactive-quiet-hours');

            var dailyRaw = dailyEl ? dailyEl.value.trim() : '';
            var dailyLimit = parseInt(dailyRaw, 10);
            if (!/^\d+$/.test(dailyRaw) || dailyLimit < 1 || dailyLimit > 10) {
                RamariaToast.show('warning', '每日投递上限需为 1~10 的整数');
                return;
            }

            var idleRaw = idleEl ? idleEl.value.trim() : '';
            var minIdle = parseInt(idleRaw, 10);
            if (!/^\d+$/.test(idleRaw) || minIdle < 1 || minIdle > 48) {
                RamariaToast.show('warning', '最小空闲时长需为 1~48 的整数（小时）');
                return;
            }

            var quietHours = quietEl ? quietEl.value.trim() : '';
            if (!NS.QUIET_HOURS_PATTERN.test(quietHours)) {
                RamariaToast.show('warning', '免打扰时段格式应为 HH:MM-HH:MM（如 22:00-08:00）');
                return;
            }

            // 深拷贝后仅改 proactive 四键，其余字段原样回写
            var cfg = JSON.parse(JSON.stringify(state.fullConfig));
            if (!cfg.proactive) cfg.proactive = {};
            cfg.proactive.enabled = !!(enabledEl && enabledEl.checked);
            cfg.proactive.daily_limit = dailyLimit;
            cfg.proactive.min_idle_hours = minIdle;
            cfg.proactive.quiet_hours = quietHours;

            var result = await RamariaApi.config.updateFull(cfg);
            if (result && result.fileOk === false && result.dbOk === false) {
                throw new Error('配置双写均失败');
            }
            // 后端保存成功后已热更新运行时参数（无需重启）
            if (!state.fullConfig.proactive) state.fullConfig.proactive = {};
            state.fullConfig.proactive.enabled = cfg.proactive.enabled;
            state.fullConfig.proactive.daily_limit = dailyLimit;
            state.fullConfig.proactive.min_idle_hours = minIdle;
            state.fullConfig.proactive.quiet_hours = quietHours;
            RamariaToast.show('success', '主动对话设置已保存（立即生效）');
        } catch (err) {
            RamariaToast.show('error', '保存主动对话设置失败', err.message || '未知错误');
        }
    }

// =========================================================
// 主动消息名单（逐人格三态开关；行内切换即时保存）
// =========================================================

    /**
     * 主动消息名单状态文案（四态）。
     *
     * 参数:
     * - `mode`: 开关三态（auto / on / off）
     * - `hasDialogue`: 是否存在本地对话（自动态是否已解锁）
     * - `kind`: 人格类型（user 行固定显示不参与说明）
     *
     * 返回:
     * - 中文状态文案；user 行控件禁用并说明"用户人格不参与主动对话"。
     */
    function _proactivePersonaStatusText(mode, hasDialogue, kind) {
        if (kind === 'user') return '用户人格不参与主动对话';
        if (mode === 'on') return '已开启（手动）';
        if (mode === 'off') return '已关闭';
        return hasDialogue ? '自动开启（已对话过）' : '未解锁（对话一次后自动开启）';
    }

    /**
     * 归一化开关文本：非 on / off 一律回退 auto（防后端异常值打断渲染）。
     */
    function _normalizeProactiveMode(mode) {
        return (mode === 'on' || mode === 'off') ? mode : 'auto';
    }

    /**
     * 名单读命令结果 → 行视图模型（纯函数）。
     *
     * 参数:
     * - `list`: list_proactive_personas 返回数组
     *
     * 返回:
     * - [{ uid, name, kind, mode, hasDialogue, statusText, selectDisabled }]；
     *   user 行 selectDisabled=true（硬排除，不可设置）。
     */
    function _buildProactivePersonaRows(list) {
        var rows = [];
        if (!Array.isArray(list)) return rows;
        for (var i = 0; i < list.length; i++) {
            var item = list[i] || {};
            if (!item.uid) continue;
            var mode = _normalizeProactiveMode(item.mode);
            var hasDialogue = !!item.has_local_dialogue;
            var kind = String(item.kind || '');
            rows.push({
                uid: String(item.uid),
                name: String(item.name || item.uid),
                kind: kind,
                mode: mode,
                hasDialogue: hasDialogue,
                statusText: _proactivePersonaStatusText(mode, hasDialogue, kind),
                selectDisabled: kind === 'user',
            });
        }
        return rows;
    }

    /**
     * 人格类型徽标中文标签（名单行展示用）。
     */
    function _proactiveKindLabel(kind) {
        var map = {
            'user': '用户',
            'rama': 'Rama',
            'char': '角色',
            'anim': '动漫',
            'oc': 'OC',
            'hist': '历史',
        };
        return map[kind] || kind;
    }

    /**
     * 名单行 HTML（全部动态字段经转义，防注入）。
     */
    function _proactivePersonaRowHtml(row) {
        var escape = RamariaEscape.escapeHtml;
        var options = '';
        var modes = [['auto', '自动'], ['on', '开启'], ['off', '关闭']];
        for (var i = 0; i < modes.length; i++) {
            options += '<option value="' + modes[i][0] + '"' +
                (row.mode === modes[i][0] ? ' selected' : '') + '>' + modes[i][1] + '</option>';
        }
        return '<div class="settings-proactive-roster-row">' +
            '<div class="settings-proactive-roster-info">' +
                '<div class="settings-proactive-roster-identity">' +
                    '<span class="settings-proactive-roster-name">' + escape(row.name) + '</span>' +
                    '<span class="settings-proactive-roster-kind">' + escape(_proactiveKindLabel(row.kind)) + '</span>' +
                '</div>' +
                '<div class="settings-proactive-roster-row-status" id="settings-proactive-status-' + escape(row.uid) + '">' +
                    escape(row.statusText) +
                '</div>' +
            '</div>' +
            '<select class="settings-form-select settings-proactive-roster-select"' +
                ' data-uid="' + escape(row.uid) + '"' +
                ' data-has-dialogue="' + (row.hasDialogue ? '1' : '0') + '"' +
                ' data-kind="' + escape(row.kind) + '"' +
                ' data-prev-mode="' + escape(row.mode) + '"' +
                (row.selectDisabled ? ' disabled' : '') + '>' +
                options +
            '</select>' +
        '</div>';
    }

    /**
     * 渲染名单列表（空态 / 行渲染 + 事件绑定）。
     */
    function _renderProactivePersonaList(rows) {
        var container = NS.$('settings-proactive-roster');
        if (!container) return;
        if (!rows.length) {
            container.innerHTML = '<div class="settings-proactive-roster-placeholder">暂无可管理的人格</div>';
            return;
        }
        var html = '';
        for (var i = 0; i < rows.length; i++) html += _proactivePersonaRowHtml(rows[i]);
        container.innerHTML = html;

        var selects = container.querySelectorAll('.settings-proactive-roster-select');
        for (var j = 0; j < selects.length; j++) {
            selects[j].addEventListener('change', _onProactivePersonaSelectChange);
        }
    }

    /**
     * 加载并渲染主动消息名单（读取失败只显示错误态，不回显旧值）。
     */
    async function _loadProactivePersonas() {
        var container = NS.$('settings-proactive-roster');
        if (container) {
            container.innerHTML = '<div class="settings-proactive-roster-placeholder">加载中…</div>';
        }
        try {
            var list = await RamariaApi.proactive.listPersonas();
            _renderProactivePersonaList(_buildProactivePersonaRows(list));
        } catch (err) {
            console.error('[SettingsView] 加载主动消息名单失败:', err);
            var current = NS.$('settings-proactive-roster');
            if (current) {
                current.innerHTML = '<div class="settings-proactive-roster-placeholder settings-proactive-roster-error">名单加载失败：' +
                    RamariaEscape.escapeHtml(err.message || '未知错误') + '</div>';
            }
        }
    }

    /**
     * 行内开关切换事件入口。
     */
    function _onProactivePersonaSelectChange(evt) {
        _handleProactivePersonaChange(evt.target);
    }

    /**
     * 行内开关切换：即时保存；成功更新状态文案，失败回滚控件并提示。
     *
     * 参数:
     * - `selectEl`: 名单行的三态控件（dataset 携带 uid / 对话状态 / 类型 / 切换前值）。
     */
    async function _handleProactivePersonaChange(selectEl) {
        if (!selectEl || !selectEl.dataset) return;
        var uid = selectEl.dataset.uid || '';
        var prev = _normalizeProactiveMode(selectEl.dataset.prevMode);
        var next = _normalizeProactiveMode(selectEl.value);
        var hasDialogue = selectEl.dataset.hasDialogue === '1';
        var kind = selectEl.dataset.kind || '';
        if (!uid || next === prev) return;

        selectEl.disabled = true;
        try {
            await RamariaApi.proactive.setPersona(uid, next);
            selectEl.dataset.prevMode = next;
            selectEl.value = next;
            _updateProactivePersonaStatus(uid, next, hasDialogue, kind);
            RamariaToast.show('success', '主动开关已保存');
            // 冷人格手动强开：给出素材提示（保存已成功，不阻塞）
            if (next === 'on' && !hasDialogue) {
                RamariaToast.show('info', '该人格尚未对话过', '主动消息可能缺少话题素材');
            }
        } catch (err) {
            // 保存失败：回滚控件与显示值，避免误显示为已生效
            selectEl.value = prev;
            RamariaToast.show('error', '保存主动开关失败', err.message || '未知错误');
        } finally {
            selectEl.disabled = false;
        }
    }

    /**
     * 更新名单行状态文案（行不存在时静默跳过，如切换后已离开设置页）。
     */
    function _updateProactivePersonaStatus(uid, mode, hasDialogue, kind) {
        var el = NS.$('settings-proactive-status-' + uid);
        if (el) el.textContent = _proactivePersonaStatusText(mode, hasDialogue, kind);
    }

 // =========================================================
 // 挂载面
 // =========================================================

    NS.parts.proactive = {
        /** 渲染主动对话目录项（四键 + 名单卡） */
        render: _renderProactiveSection,
        /** 回显主动对话四键 */
        fill: _fillProactiveForm,
        /** 加载并渲染主动消息名单（供回归测试直接驱动） */
        loadPersonas: _loadProactivePersonas,
        /** 名单行视图模型（纯函数，供回归测试直接驱动） */
        buildRows: _buildProactivePersonaRows,
        /** 名单状态文案（纯函数） */
        statusText: _proactivePersonaStatusText,
        /** 名单行开关切换处理器（供回归测试直接驱动） */
        handlePersonaChange: _handleProactivePersonaChange,
    };
})(window.RamariaSettingsNS);
