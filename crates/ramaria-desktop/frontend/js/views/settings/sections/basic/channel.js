/**
 * js/views/settings/sections/basic/channel.js — Ramaria 设置页「社交平台接入」目录项
 *
 * 职责:
 * - 通道状态卡（启停 / 各平台连接状态与最近错误 / 凭据配置标记）
 * - 通道配置（6 键，数据驱动渲染）
 * - 平台凭据写入（OS keychain，仅回显"已配置 / 未配置"）
 * - 会话白名单（逐会话放行 / 改绑人格 / 允许主动；行内变更即时写绑定）
 *
 * 设计特点:
 * - 面板字段元数据与高级设置页 channel 组逐键一致（回归测试锁定默认值防漂移）
 * - 凭据输入留空 = 不修改；绑定变更失败回读服务端状态，保证展示与实际一致
 * - 通道状态事件在设置页存续期间订阅，离开视图时统一清理
 *
 * 依赖:
 * - NS.$ / NS.handleUpdateFullResult（shared.js）
 * - RamariaApi / RamariaToast / RamariaEscape / TauriBridge
 */

(function (NS) {
    'use strict';

    var state = NS.state;

// =========================================================
// 社交平台接入区块
// =========================================================

    /**
     * 「社交平台接入」面板字段元数据（数据驱动渲染 / 回显 / 收集）。
     *
     * 字段约定（与 `_ADVANCED_GROUPS` 的字段同构）:
     * - `path`: 组内相对路径；完整配置路径 = ['channel'].concat(path)。
     * - `def`: 默认值，与 `config/default.toml` 的 `[channel]` 组逐键一致（回归测试锁定）。
     *
     * 说明:
     * - 高级设置页的 `channel` 组与本面板编辑同一组配置键；两处保存都经
     *   `update_full_config` 统一写入口（本面板用于日常开关，高级页保留完整视图）。
     * - 平台凭据（OneBot access token / Telegram bot token）不在本组：经 OS keychain
     *   存储，界面仅显示"已配置 / 未配置"，不回显明文。
     */
    var _CHANNEL_FIELDS = [
        { path: ['enabled'], label: '通道总开关', type: 'bool', def: false, hint: '关闭时不连接任何平台' },
        { path: ['qq_enabled'], label: 'QQ 平台（本地 OneBot 协议端）', type: 'bool', def: false, hint: '需通道总开关开启；协议端（如 NapCat）需先行启动' },
        { path: ['qq_ws_url'], label: 'QQ WebSocket 地址', type: 'text', def: 'ws://127.0.0.1:3001', hint: 'OneBot v11 正向 WebSocket 地址（协议端服务端口）' },
        { path: ['telegram_enabled'], label: 'Telegram 平台（Bot API）', type: 'bool', def: false, hint: '需通道总开关开启；需已配置 Bot 令牌' },
        { path: ['default_persona_uid'], label: '默认本机侧人格', type: 'text', def: 'rama-0001', hint: '绑定未覆盖时使用的对话人格 uid' },
        { path: ['proactive_delivery_enabled'], label: '社交主动投递开关', type: 'bool', def: false, hint: '主动消息投递到社交通道的总开关；还须绑定级开关放行' },
    ];

    /**
     * 通道面板字段的 DOM id（与元数据路径一一对应）。
     */
    function _channelFieldId(f) {
        return 'settings-channel-' + f.path.join('-');
    }

    /**
     * 通道连接状态 → 展示视图（中文标签 + 语义分级）。
     *
     * 参数:
     * - `state`: 服务端状态字符串（disconnected / connecting / online / error）。
     *
     * 返回:
     * - `{ label, level }`: 徽标文案与语义级别（ok / warn / danger / neutral）。
     */
    function _channelStateView(state) {
        switch (state) {
            case 'online': return { label: '在线', level: 'ok' };
            case 'connecting': return { label: '连接中', level: 'warn' };
            case 'error': return { label: '错误', level: 'danger' };
            case 'disconnected': return { label: '已断开', level: 'neutral' };
            default: return { label: '未知', level: 'neutral' };
        }
    }

    /**
     * 通道名 → 展示标签。
     */
    function _channelLabel(channel) {
        if (channel === 'qq') return 'QQ';
        if (channel === 'telegram') return 'Telegram';
        return String(channel);
    }

    /**
     * 外部会话标识 → 展示文案（私聊 / 群聊 前缀归一）。
     */
    function _channelRefLabel(externalRef) {
        var ref = String(externalRef || '');
        if (ref.indexOf('c2c:') === 0) return '私聊 · ' + ref.slice(4);
        if (ref.indexOf('group:') === 0) return '群聊 · ' + ref.slice(6);
        return ref;
    }

    /**
     * 绑定列表读命令结果 → 行视图模型（纯函数）。
     *
     * 参数:
     * - `bindings`: list_channel_bindings 返回数组。
     *
     * 返回:
     * - [{ channel, externalRef, channelLabel, refLabel, personaUid, effectivePersonaUid,
     *     enabled, allowProactive }]；缺关键字段的行跳过（防御后端异常数据）。
     */
    function _channelBindingRows(bindings) {
        var rows = [];
        if (!Array.isArray(bindings)) return rows;
        for (var i = 0; i < bindings.length; i++) {
            var item = bindings[i] || {};
            if (!item.channel || !item.externalRef) continue;
            rows.push({
                channel: String(item.channel),
                externalRef: String(item.externalRef),
                channelLabel: _channelLabel(item.channel),
                refLabel: _channelRefLabel(item.externalRef),
                personaUid: item.personaUid ? String(item.personaUid) : null,
                effectivePersonaUid: item.effectivePersonaUid ? String(item.effectivePersonaUid) : '',
                enabled: !!item.enabled,
                allowProactive: !!item.allowProactive,
            });
        }
        return rows;
    }

    /**
     * 平台 → 凭据设置保留键（经既有设置命令面分流写入 OS keychain）。
     *
     * 返回:
     * - 保留键字符串；未知平台返回空串（调用方据此拒绝保存）。
     */
    function _channelCredentialKey(platform) {
        if (platform === 'qq') return 'channel.qq_access_token';
        if (platform === 'telegram') return 'channel.telegram_bot_token';
        return '';
    }

    /**
     * 将面板表单值合并进完整配置（纯函数；返回新对象，不修改入参）。
     *
     * 参数:
     * - `config`: 完整配置对象（get_full_config 结果）。
     * - `values`: 面板收集的 `[channel]` 组值（6 键）。
     */
    function _applyChannelFormToConfig(config, values) {
        var cfg = JSON.parse(JSON.stringify(config || {}));
        if (!cfg.channel || typeof cfg.channel !== 'object') cfg.channel = {};
        for (var i = 0; i < _CHANNEL_FIELDS.length; i++) {
            var key = _CHANNEL_FIELDS[i].path[0];
            if (values && values[key] !== undefined) cfg.channel[key] = values[key];
        }
        return cfg;
    }

    /**
     * 渲染一个通道面板字段的 HTML（由元数据驱动，与回显 / 收集共用同一 id 规则）。
     */
    function _channelFieldHtml(f) {
        var fid = _channelFieldId(f);

        if (f.type === 'bool') {
            return '<div class="settings-form-group">' +
                '<label class="settings-form-label">' +
                    '<input type="checkbox" id="' + fid + '"' + (f.def ? ' checked' : '') + ' /> ' + f.label +
                '</label>' +
                '<div class="settings-form-hint">' + f.hint + '（默认：' + (f.def ? '开' : '关') + '）</div>' +
            '</div>';
        }

        return '<div class="settings-form-group">' +
            '<label class="settings-form-label">' + f.label + '</label>' +
            '<input class="settings-form-input" id="' + fid + '" type="text" value="' + f.def + '" />' +
            '<div class="settings-form-hint">' + f.hint + '（默认：' + f.def + '）</div>' +
        '</div>';
    }

    /**
     * 回显通道配置（进入设置页时调用；缺字段不覆盖当前控件值）。
     */
    function _fillChannelForm(config) {
        if (!config || !config.channel) return;
        var ch = config.channel;

        for (var i = 0; i < _CHANNEL_FIELDS.length; i++) {
            var f = _CHANNEL_FIELDS[i];
            var el = NS.$(_channelFieldId(f));
            if (!el) continue;
            var key = f.path[0];
            if (f.type === 'bool') {
                if (typeof ch[key] === 'boolean') el.checked = ch[key];
            } else if (ch[key] !== undefined && ch[key] !== null) {
                el.value = String(ch[key]);
            }
        }
    }

    /**
     * 从面板收集通道配置。
     *
     * 返回:
     * - `{ values: {...} }`: 收集成功（可合并进完整配置）；
     * - `{ error: '...' }`: 校验失败（文本字段为空），调用方提示后中止保存。
     */
    function _collectChannelConfig() {
        var values = {};

        for (var i = 0; i < _CHANNEL_FIELDS.length; i++) {
            var f = _CHANNEL_FIELDS[i];
            var el = NS.$(_channelFieldId(f));
            var key = f.path[0];

            if (f.type === 'bool') {
                values[key] = !!(el && el.checked);
            } else {
                var raw = el ? String(el.value).trim() : '';
                if (!raw) {
                    return { error: f.label + ' 不能为空' };
                }
                values[key] = raw;
            }
        }

        return { values: values };
    }

    /**
     * 渲染「社交平台接入」区块。
     *
     * 结构:
     * - 状态卡（运行标记 / 各平台连接状态与最近错误 / 启停按钮）；
     * - 风险提示条（外发与协议端依赖说明）；
     * - 配置卡（6 键，元数据驱动）；
     * - 凭据卡（两平台令牌输入 + 已配置标记回显，不回显明文）；
     * - 绑定管理卡（列表 / 改绑 / 开关）。
     */
    function _renderChannelSection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">💬 社交平台接入</div>' +
            '<div class="settings-section-desc">' +
                '通过本地协议端（QQ / Telegram）与外部平台收发消息，消息进入记忆链路（默认关闭）。' +
            '</div>';

        // ── 状态卡（只读展示 + 启停）──
        var statusCard = document.createElement('div');
        statusCard.className = 'settings-card';
        statusCard.innerHTML =
            '<div class="settings-row">' +
                '<div>' +
                    '<div class="settings-row-label">运行状态</div>' +
                    '<div class="settings-row-meta">通道随桌面启动按配置连接；双开时仅一个实例持有连接</div>' +
                '</div>' +
                '<span class="settings-row-value" id="settings-channel-running">未运行</span>' +
            '</div>' +
            '<div class="settings-row">' +
                '<div>' +
                    '<div class="settings-row-label">QQ</div>' +
                    '<div class="settings-row-meta" id="settings-channel-qq-error"></div>' +
                '</div>' +
                '<span class="settings-row-value" id="settings-channel-qq-state">未配置</span>' +
            '</div>' +
            '<div class="settings-row">' +
                '<div>' +
                    '<div class="settings-row-label">Telegram</div>' +
                    '<div class="settings-row-meta" id="settings-channel-telegram-error"></div>' +
                '</div>' +
                '<span class="settings-row-value" id="settings-channel-telegram-state">未配置</span>' +
            '</div>' +
            '<div class="settings-actions">' +
                '<button class="btn btn-secondary btn-sm" id="settings-channel-start">启动</button>' +
                '<button class="btn btn-secondary btn-sm" id="settings-channel-stop">停止</button>' +
                '<button class="btn btn-secondary btn-sm" id="settings-channel-refresh">刷新状态</button>' +
            '</div>';
        section.appendChild(statusCard);

        // ── 风险提示（外发与协议端依赖）──
        var risk = document.createElement('div');
        risk.className = 'settings-risk-banner';
        risk.textContent =
            '⚠️ 启用后，外部平台消息与图片会进入本机记忆库；使用线上模型时相关内容会随对话外发。' +
            'QQ 通道需要本地 OneBot 协议端（如 NapCat）保持运行。';
        section.appendChild(risk);

        // ── 配置卡（字段由元数据驱动渲染）──
        var formCard = document.createElement('div');
        formCard.className = 'settings-card';
        var html = '';
        for (var i = 0; i < _CHANNEL_FIELDS.length; i++) {
            html += _channelFieldHtml(_CHANNEL_FIELDS[i]);
        }
        html += '<div class="settings-save-hint">' +
                    '<button class="btn btn-primary btn-sm" id="settings-channel-config-save">保存通道配置</button>' +
                '</div>';
        formCard.innerHTML = html;
        section.appendChild(formCard);

        // ── 凭据卡（写系统凭据管理器；仅回显"已配置 / 未配置"）──
        var credCard = document.createElement('div');
        credCard.className = 'settings-card';
        credCard.innerHTML =
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">QQ Access Token</label>' +
                '<form id="settings-channel-qq-token-form" autocomplete="off">' +
                    '<input class="settings-form-input" id="settings-channel-qq-token" type="password" ' +
                        'autocomplete="new-password" placeholder="未配置" />' +
                '</form>' +
                '<div class="settings-form-hint" id="settings-channel-qq-token-state">状态未知</div>' +
                '<div class="settings-actions">' +
                    '<button class="btn btn-secondary btn-sm" id="settings-channel-qq-token-save">保存 QQ Token</button>' +
                '</div>' +
            '</div>' +
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">Telegram Bot Token</label>' +
                '<form id="settings-channel-telegram-token-form" autocomplete="off">' +
                    '<input class="settings-form-input" id="settings-channel-telegram-token" type="password" ' +
                        'autocomplete="new-password" placeholder="未配置" />' +
                '</form>' +
                '<div class="settings-form-hint" id="settings-channel-telegram-token-state">状态未知</div>' +
                '<div class="settings-actions">' +
                    '<button class="btn btn-secondary btn-sm" id="settings-channel-telegram-token-save">保存 Telegram Token</button>' +
                '</div>' +
            '</div>' +
            '<div class="settings-form-hint">' +
                '凭据保存在系统凭据管理器（不进配置文件、不回显明文）；' +
                '协议端未开启鉴权时 QQ Token 可留空。修改后需重新「启动」通道生效。' +
            '</div>';
        section.appendChild(credCard);

        // ── 绑定管理卡（列表 / 改绑 / 开关）──
        var bindCard = document.createElement('div');
        bindCard.className = 'settings-card';
        bindCard.innerHTML =
            '<div class="settings-row-label">会话白名单</div>' +
            '<div class="settings-row-meta">' +
                '收到消息时自动建档，但默认不放行；仅在此显式放行的会话会被处理并进入记忆' +
                '（未放行的消息不入库、不回复）。' +
                '一个群 = 一个会话，群聊仅在被 @ 或回复时响应；生效人格 = 绑定覆盖 → 通道默认 → 兜底 rama-0001。' +
            '</div>' +
            '<div id="settings-channel-bindings">' +
                '<div class="settings-form-hint">加载中...</div>' +
            '</div>' +
            '<div class="settings-actions">' +
                '<button class="btn btn-secondary btn-sm" id="settings-channel-bindings-refresh">刷新绑定</button>' +
            '</div>';
        section.appendChild(bindCard);

        parent.appendChild(section);

        // ── 事件绑定 ──
        var startBtn = NS.$('settings-channel-start');
        if (startBtn) startBtn.addEventListener('click', _handleChannelStart);

        var stopBtn = NS.$('settings-channel-stop');
        if (stopBtn) stopBtn.addEventListener('click', _handleChannelStop);

        var refreshBtn = NS.$('settings-channel-refresh');
        if (refreshBtn) {
            refreshBtn.addEventListener('click', function () {
                RamariaToast.show('info', '正在刷新通道状态...');
                _loadChannelStatus();
            });
        }

        var configSaveBtn = NS.$('settings-channel-config-save');
        if (configSaveBtn) configSaveBtn.addEventListener('click', _handleSaveChannelConfig);

        var qqTokenSaveBtn = NS.$('settings-channel-qq-token-save');
        if (qqTokenSaveBtn) {
            qqTokenSaveBtn.addEventListener('click', function () {
                _handleSaveChannelCredential('qq');
            });
        }

        var tgTokenSaveBtn = NS.$('settings-channel-telegram-token-save');
        if (tgTokenSaveBtn) {
            tgTokenSaveBtn.addEventListener('click', function () {
                _handleSaveChannelCredential('telegram');
            });
        }

        var bindingsRefreshBtn = NS.$('settings-channel-bindings-refresh');
        if (bindingsRefreshBtn) {
            bindingsRefreshBtn.addEventListener('click', function () {
                _loadChannelBindings();
            });
        }

        var bindingsBox = NS.$('settings-channel-bindings');
        if (bindingsBox) bindingsBox.addEventListener('change', _handleChannelBindingChange);

        // 凭据表单：拦截提交（防止 Enter 触发页面级提交行为）
        var qqForm = NS.$('settings-channel-qq-token-form');
        if (qqForm) qqForm.addEventListener('submit', function (ev) { ev.preventDefault(); });
        var tgForm = NS.$('settings-channel-telegram-token-form');
        if (tgForm) tgForm.addEventListener('submit', function (ev) { ev.preventDefault(); });
    }

    /**
     * 加载社交通道面板全部数据（状态 + 人格列表 + 绑定列表）。
     */
    async function _loadChannelPanel() {
        await _loadChannelStatus();
        await _loadChannelPersonas();
        await _loadChannelBindings();
    }

    /**
     * 加载通道状态（失败按空处理并提示"查询失败"，不阻塞面板其余操作）。
     */
    async function _loadChannelStatus() {
        try {
            state.channelStatus = await RamariaApi.channel.getStatus();
        } catch (err) {
            console.error('[SettingsView] 查询社交通道状态失败:', err);
            state.channelStatus = null;
        }
        _fillChannelStatus(state.channelStatus);
    }

    /**
     * 加载绑定列表（失败按空处理）。
     */
    async function _loadChannelBindings() {
        try {
            var list = await RamariaApi.channel.listBindings();
            state.channelBindings = Array.isArray(list) ? list : [];
        } catch (err) {
            console.error('[SettingsView] 查询通道绑定失败:', err);
            state.channelBindings = [];
        }
        _renderChannelBindingList();
    }

    /**
     * 加载改绑下拉可选人格（一次加载后缓存；失败按空处理，下拉仅保留生效项）。
     */
    async function _loadChannelPersonas() {
        if (state.channelPersonas.length) return;
        try {
            var list = await RamariaApi.persona.listFull();
            state.channelPersonas = Array.isArray(list) ? list : [];
        } catch (err) {
            console.error('[SettingsView] 查询人格列表失败:', err);
            state.channelPersonas = [];
        }
    }

    /**
     * 回显通道状态卡（运行标记 / 各平台状态与最近错误 / 凭据配置标记）。
     */
    function _fillChannelStatus(status) {
        var runningEl = NS.$('settings-channel-running');

        if (!status) {
            if (runningEl) runningEl.textContent = '查询失败';
            _fillChannelPlatformState(null, 'qq', NS.$('settings-channel-qq-state'), NS.$('settings-channel-qq-error'));
            _fillChannelPlatformState(null, 'telegram', NS.$('settings-channel-telegram-state'), NS.$('settings-channel-telegram-error'));
            return;
        }

        if (runningEl) {
            runningEl.textContent = status.running ? '运行中' : '未运行';
        }
        _fillChannelPlatformState(status, 'qq', NS.$('settings-channel-qq-state'), NS.$('settings-channel-qq-error'));
        _fillChannelPlatformState(status, 'telegram', NS.$('settings-channel-telegram-state'), NS.$('settings-channel-telegram-error'));

        // 凭据配置标记（只显示"已配置 / 未配置"，不回显明文）
        var qqTokenState = NS.$('settings-channel-qq-token-state');
        if (qqTokenState) {
            qqTokenState.textContent = status.qqTokenConfigured
                ? '已配置（输入留空保存不修改）'
                : '未配置（协议端未开启鉴权可留空）';
        }
        var tgTokenState = NS.$('settings-channel-telegram-token-state');
        if (tgTokenState) {
            tgTokenState.textContent = status.telegramTokenConfigured
                ? '已配置（输入留空保存不修改）'
                : '未配置';
        }
        var qqTokenInput = NS.$('settings-channel-qq-token');
        if (qqTokenInput) {
            qqTokenInput.placeholder = status.qqTokenConfigured ? '已配置（留空不修改）' : '未配置';
        }
        var tgTokenInput = NS.$('settings-channel-telegram-token');
        if (tgTokenInput) {
            tgTokenInput.placeholder = status.telegramTokenConfigured ? '已配置（留空不修改）' : '未配置';
        }
    }

    /**
     * 回显单平台状态格（启用标记 / 连接状态 / 最近错误）。
     */
    function _fillChannelPlatformState(status, channel, stateEl, errEl) {
        if (!stateEl && !errEl) return;

        if (!status) {
            if (stateEl) stateEl.textContent = '-';
            if (errEl) errEl.textContent = '';
            return;
        }

        var configured = channel === 'qq' ? status.qqEnabled : status.telegramEnabled;
        var entry = null;
        var list = Array.isArray(status.channels) ? status.channels : [];
        for (var i = 0; i < list.length; i++) {
            if (list[i] && list[i].channel === channel) {
                entry = list[i];
                break;
            }
        }

        if (stateEl) {
            if (!status.enabled) {
                stateEl.textContent = '总开关已关闭';
            } else if (!configured) {
                stateEl.textContent = '未启用';
            } else if (entry) {
                stateEl.textContent = _channelStateView(entry.state).label;
            } else {
                stateEl.textContent = status.running ? '未连接' : '未运行';
            }
        }
        if (errEl) {
            errEl.textContent = entry && entry.lastError ? ('最近错误：' + entry.lastError) : '';
        }
    }

    /**
     * 渲染绑定列表（空态 / 行渲染；事件由容器 change 委托统一处理）。
     */
    function _renderChannelBindingList() {
        var container = NS.$('settings-channel-bindings');
        if (!container) return;

        var rows = _channelBindingRows(state.channelBindings);
        if (!rows.length) {
            container.innerHTML = '<div class="settings-form-hint">暂无绑定（收到外部消息后自动建档）</div>';
            return;
        }

        var html = '';
        for (var i = 0; i < rows.length; i++) {
            html += _channelBindingRowHtml(rows[i]);
        }
        container.innerHTML = html;
    }

    /**
     * 单条绑定行 HTML（全部动态字段经转义，防注入）。
     */
    function _channelBindingRowHtml(row) {
        var escape = RamariaEscape.escapeHtml;
        var options = '';
        var matched = false;

        for (var i = 0; i < state.channelPersonas.length; i++) {
            var p = state.channelPersonas[i] || {};
            if (!p.uid) continue;
            var selected = row.personaUid
                ? row.personaUid === p.uid
                : row.effectivePersonaUid === p.uid;
            if (selected) matched = true;
            options += '<option value="' + escape(String(p.uid)) + '"' + (selected ? ' selected' : '') + '>' +
                escape(String(p.name || p.uid)) + '（' + escape(String(p.uid)) + '）</option>';
        }
        // 生效人格不在人格列表内（防御）：补一项，保证下拉显示与当前生效一致
        if (!matched && row.effectivePersonaUid) {
            options = '<option value="' + escape(row.effectivePersonaUid) + '" selected>' +
                escape(row.effectivePersonaUid) + '</option>' + options;
        }

        var scopeLabel = row.personaUid ? '绑定覆盖' : '继承通道默认';
        return '<div class="settings-form-group settings-channel-binding-row"' +
                ' data-channel="' + escape(row.channel) + '"' +
                ' data-external-ref="' + escape(row.externalRef) + '">' +
            '<div class="settings-row-label">' + escape(row.channelLabel) + ' · ' + escape(row.refLabel) + '</div>' +
            '<div class="settings-form-hint">生效人格：' + escape(row.effectivePersonaUid || '（未知）') +
                '（' + scopeLabel + '）</div>' +
            '<div class="settings-form-whitelist">' +
                '<select class="settings-form-select" data-role="persona" title="修改绑定人格">' +
                    options +
                '</select>' +
                '<label class="settings-form-inline-label">' +
                    '<input type="checkbox" data-role="enabled"' + (row.enabled ? ' checked' : '') + ' /> 放行' +
                '</label>' +
                '<label class="settings-form-inline-label">' +
                    '<input type="checkbox" data-role="allow-proactive"' + (row.allowProactive ? ' checked' : '') + ' /> 允许主动' +
                '</label>' +
            '</div>' +
        '</div>';
    }

    /**
     * 绑定行控件变更 → 写绑定（失败回读服务端状态，保证展示与实际一致）。
     */
    async function _handleChannelBindingChange(ev) {
        var el = ev && ev.target;
        if (!el || typeof el.getAttribute !== 'function') return;
        var role = el.getAttribute('data-role');
        if (!role) return;

        var rowEl = el.closest ? el.closest('.settings-channel-binding-row') : null;
        if (!rowEl) return;
        var channel = rowEl.getAttribute('data-channel');
        var externalRef = rowEl.getAttribute('data-external-ref');
        if (!channel || !externalRef) return;

        var personaUid = null;
        var enabled = null;
        var allowProactive = null;
        if (role === 'enabled') {
            enabled = !!el.checked;
        } else if (role === 'allow-proactive') {
            allowProactive = !!el.checked;
        } else if (role === 'persona') {
            if (!el.value) return;
            personaUid = el.value;
        } else {
            return;
        }

        try {
            await RamariaApi.channel.setBinding(channel, externalRef, personaUid, enabled, allowProactive);
            RamariaToast.show('success', '绑定已更新');
        } catch (err) {
            RamariaToast.show('error', '绑定更新失败', err.message || '未知错误');
        }
        await _loadChannelBindings();
    }

    /**
     * 启动通道运行时（幂等；配置未启用 / 互斥占用时状态卡如实展示）。
     */
    async function _handleChannelStart() {
        var btn = NS.$('settings-channel-start');
        if (btn) btn.disabled = true;
        try {
            RamariaToast.show('info', '正在启动社交通道...');
            var status = await RamariaApi.channel.start();
            state.channelStatus = status || null;
            _fillChannelStatus(state.channelStatus);
            RamariaToast.show('success', '社交通道启动操作完成');
        } catch (err) {
            RamariaToast.show('error', '启动失败', err.message || '未知错误');
        } finally {
            if (btn) btn.disabled = false;
        }
    }

    /**
     * 停止通道运行时（幂等）。
     */
    async function _handleChannelStop() {
        var btn = NS.$('settings-channel-stop');
        if (btn) btn.disabled = true;
        try {
            var status = await RamariaApi.channel.stop();
            state.channelStatus = status || null;
            _fillChannelStatus(state.channelStatus);
            RamariaToast.show('success', '社交通道已停止');
        } catch (err) {
            RamariaToast.show('error', '停止失败', err.message || '未知错误');
        } finally {
            if (btn) btn.disabled = false;
        }
    }

    /**
     * 保存通道配置（统一写入口双写；保存后刷新状态卡）。
     */
    async function _handleSaveChannelConfig() {
        try {
            if (!state.fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }

            var collected = _collectChannelConfig();
            if (collected.error) {
                RamariaToast.show('warning', collected.error);
                return;
            }

            var cfg = _applyChannelFormToConfig(state.fullConfig, collected.values);
            var result = await RamariaApi.config.updateFull(cfg);
            if (!NS.handleUpdateFullResult(result, '通道配置已保存（重新「启动」后按新配置连接）')) {
                throw new Error('配置双写均失败');
            }
            state.fullConfig = cfg;
            await _loadChannelStatus();
        } catch (err) {
            RamariaToast.show('error', '保存失败', err.message || '未知错误');
        }
    }

    /**
     * 保存平台凭据（输入框取值 → 保留键经既有设置命令面写入 keychain）。
     *
     * 说明:
     * - 输入留空 = 不修改（已有凭据不回显，无需重填）；
     * - 保存成功后清空输入框并刷新"已配置"标记。
     */
    async function _handleSaveChannelCredential(platform) {
        var inputId = platform === 'qq' ? 'settings-channel-qq-token' : 'settings-channel-telegram-token';
        var input = NS.$(inputId);
        var value = input ? String(input.value).trim() : '';
        if (!value) {
            RamariaToast.show('warning', '请输入新 Token（输入留空不修改已有凭据）');
            return;
        }

        var btnId = platform === 'qq' ? 'settings-channel-qq-token-save' : 'settings-channel-telegram-token-save';
        var btn = NS.$(btnId);
        if (btn) btn.disabled = true;
        try {
            await _saveChannelCredential(platform, value);
            if (input) input.value = '';
            RamariaToast.show('success', _channelLabel(platform) + ' 凭据已保存到系统凭据管理器');
            await _loadChannelStatus();
        } catch (err) {
            RamariaToast.show('error', '凭据保存失败', err.message || '未知错误');
        } finally {
            if (btn) btn.disabled = false;
        }
    }

    /**
     * 写入平台凭据（保留键分流到 keychain 的服务端实现见设置用例）。
     */
    async function _saveChannelCredential(platform, value) {
        var key = _channelCredentialKey(platform);
        if (!key) throw new Error('未知平台: ' + platform);
        if (!value) throw new Error('凭据不能为空');
        return await RamariaApi.config.updateSetting(key, value);
    }

    /**
     * 订阅通道状态事件（设置页存续期间有效；离开视图时随 state.unsubs 统一清理）。
     *
     * 说明:
     * - 事件负载含全部通道状态；面板存在时刷新状态展示，否则忽略；
     * - 非 Tauri 环境（测试 / 浏览器预览）无事件通道，静默跳过。
     */
    function _subscribeChannelStatus() {
        if (typeof TauriBridge === 'undefined' || !TauriBridge.listen) return;
        TauriBridge.listen('channel-status', function () {
            if (NS.$('settings-channel-running')) {
                _loadChannelStatus();
            }
        }).then(function (unlisten) {
            if (typeof unlisten === 'function') state.unsubs.push(unlisten);
        }).catch(function (err) {
            console.warn('[SettingsView] 订阅通道状态事件失败:', err);
        });
    }

 // =========================================================
 // 挂载面
 // =========================================================

    NS.parts.channel = {
        /** 渲染社交平台接入目录项（状态 / 配置 / 凭据 / 白名单四卡） */
        render: _renderChannelSection,
        /** 加载通道面板全部数据（状态 + 人格列表 + 绑定列表） */
        loadPanel: _loadChannelPanel,
        /** 订阅通道状态事件（离开视图时随订阅集合统一清理） */
        subscribeStatus: _subscribeChannelStatus,
        /** 面板字段元数据（默认值一致性回归读取） */
        fields: _CHANNEL_FIELDS,
        /** 绑定行视图模型（纯函数，供回归测试直接驱动） */
        buildBindingRows: _channelBindingRows,
        /** 连接状态展示视图（纯函数，供回归测试直接驱动） */
        stateView: _channelStateView,
        /** 平台凭据保留键（纯函数，供回归测试直接驱动） */
        credentialKey: _channelCredentialKey,
        /** 面板表单值合并完整配置（纯函数，供回归测试直接驱动） */
        applyFormToConfig: _applyChannelFormToConfig,
        /** 写入平台凭据（供回归测试直接驱动） */
        saveCredential: _saveChannelCredential,
        /** 凭据保存处理器（供回归测试直接驱动；空输入仅提示不写入） */
        handleSaveCredential: _handleSaveChannelCredential,
    };
})(window.RamariaSettingsNS);
