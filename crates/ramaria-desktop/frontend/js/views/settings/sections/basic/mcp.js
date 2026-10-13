/**
 * js/views/settings/sections/basic/mcp.js — Ramaria 设置页「MCP 接入」目录项
 *
 * 职责:
 * - MCP 挂载设置（7 键，数据驱动渲染 / 回显 / 收集）
 * - 运行状态展示（接入状态 / 最近外部活动 / 活跃会话 / 库路径）
 * - 客户端配置片段生成与复制（通用 JSON + DeepSeek Harness YAML）
 *
 * 设计特点:
 * - 面板字段元数据与 config/default.toml 的 [mcp] 组逐键一致（回归测试锁定）
 * - 白名单语义对齐服务端：['*'] 或空数组均视为"全部人格可见"
 * - 片段生成纯函数与回归测试共用；性能类输入复用高级设置默认值标注
 *
 * 依赖:
 * - NS.$ / NS.handleUpdateFullResult / NS.syncAdvancedInputDefault / NS.copyText（shared.js）
 * - RamariaApi / RamariaToast / RamariaFormat
 */

(function (NS) {
    'use strict';

    var state = NS.state;

// =========================================================
// MCP 接入区块（v2.1 M5：外部客户端挂载面板）
// =========================================================

    /**
     * 「MCP 接入」面板字段元数据（数据驱动渲染 / 回显 / 保存 / 默认值回归比对）。
     *
     * 字段约定（与 `_ADVANCED_GROUPS` 的字段同构）:
     * - `path`: 组内相对路径；完整配置路径 = ['mcp'].concat(path)。
     * - `type`: `bool` | `number` | `string-list`。
     * - `def`: 默认值，与 `config/default.toml` 的 `[mcp]` 组逐键一致（回归测试锁定）。
     * - `min` / `max`: `number` 类型的前端校验边界（服务端仍会独立钳制）。
     *
     * 说明:
     * - `allowed_personas`（`string-list`）交互为「全部人格可见」复选框 + uid 列表输入框，
     *   渲染与收集逻辑单独处理；元数据 def 仍与模板一致，纳入默认值回归测试。
     */
    var _MCP_FIELDS = [
        {
            path: ['enabled'],
            label: '启用 MCP 接入',
            type: 'bool',
            def: false,
            hint: '总开关；关闭时外部客户端挂载成功但所有工具均不可用（返回可操作错误）',
        },
        {
            path: ['allow_ingest'],
            label: '允许外部写入回流',
            type: 'bool',
            def: true,
            hint: '允许外部对话经 chat_ingest / chat_send 写回 Ramaria（桌面会话列表可见）',
        },
        {
            path: ['allow_seal'],
            label: '允许 MCP 侧封存与摘要',
            type: 'bool',
            def: true,
            hint: '封存会调用 LLM 生成 L1 摘要并改变记忆状态；关闭后只写不封存',
        },
        {
            path: ['allow_raw_text'],
            label: '允许返回原文块',
            type: 'bool',
            def: false,
            hint: '原文是最高敏感层，默认不出端；开启后原文片段可能随对话发送给客户端所用模型',
        },
        {
            path: ['allowed_personas'],
            label: '可见人格白名单',
            type: 'string-list',
            def: ['*'],
            hint: '「*」表示全部人格可见；取消「全部人格可见」后输入具体人格 uid（逗号分隔）',
        },
        {
            path: ['max_items'],
            label: '召回条目上限',
            type: 'number',
            min: 1,
            max: 20,
            def: 5,
            hint: 'memory_recall 默认返回条目数（1~20，超出按 20 截断）',
        },
        {
            path: ['max_chars'],
            label: '上下文文本预算（字符）',
            type: 'number',
            min: 100,
            max: 20000,
            def: 1200,
            hint: 'memory_recall 返回 context 的字符预算（默认 1200）',
        },
    ];

    /**
     * MCP 面板字段的 DOM id（与元数据路径一一对应）。
     */
    function _mcpFieldId(f) {
        return 'settings-mcp-' + f.path.join('-');
    }

    function _renderMcpSection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">📡 MCP 接入</div>' +
            '<div class="settings-section-desc">' +
                '让 DeepSeek Harness（dsh）/ CodeBuddy / Trae 等客户端按 MCP 协议挂载 Ramaria 的记忆与人格。' +
            '</div>';

        // ── 运行状态卡（只读展示；MCP 服务由客户端按需拉起，以通道活动近似呈现）──
        var statusCard = document.createElement('div');
        statusCard.className = 'settings-card';
        statusCard.innerHTML =
            '<div class="settings-row">' +
                '<div>' +
                    '<div class="settings-row-label">接入状态</div>' +
                    '<div class="settings-row-meta">配置开关状态；stdio 服务由客户端挂载时启动</div>' +
                '</div>' +
                '<span class="settings-row-value" id="settings-mcp-status">加载中...</span>' +
            '</div>' +
            '<div class="settings-row">' +
                '<div>' +
                    '<div class="settings-row-label">最近外部活动</div>' +
                    '<div class="settings-row-meta">外部客户端最近一次写入消息的时间</div>' +
                '</div>' +
                '<span class="settings-row-value" id="settings-mcp-activity">-</span>' +
            '</div>' +
            '<div class="settings-row">' +
                '<div>' +
                    '<div class="settings-row-label">活跃外部会话</div>' +
                    '<div class="settings-row-meta">未封存的 mcp 通道会话数</div>' +
                '</div>' +
                '<span class="settings-row-value" id="settings-mcp-active-sessions">-</span>' +
            '</div>' +
            '<div class="settings-row">' +
                '<div>' +
                    '<div class="settings-row-label">数据库路径</div>' +
                    '<div class="settings-row-meta">客户端配置片段中的 --db 参数</div>' +
                '</div>' +
                '<span class="settings-row-value settings-code-inline settings-mcp-path" id="settings-mcp-db-path">-</span>' +
            '</div>' +
            '<div class="settings-actions">' +
                '<button class="btn btn-secondary btn-sm" id="settings-mcp-copy-db-path">复制库路径</button>' +
                '<button class="btn btn-secondary btn-sm" id="settings-mcp-refresh">刷新状态</button>' +
            '</div>';
        section.appendChild(statusCard);

        // ── 隐私提示（挂载后内容随对话发送给客户端所用模型）──
        var risk = document.createElement('div');
        risk.className = 'settings-risk-banner';
        risk.textContent =
            '⚠️ 挂载 MCP 后，被检索到的记忆与外部对话回流内容会随对话发送给该客户端所使用的模型；' +
            '原文块默认不出端，可用下方人格白名单与原文开关收紧可见范围。' +
            '使用线上模型且尚未完成隐私确认时，chat_send 会被拒绝——' +
            '可在外部客户端调用 privacy_confirm 工具完成确认（与桌面隐私确认等效）。';
        section.appendChild(risk);

        // ── 配置卡（字段由元数据驱动渲染）──
        var formCard = document.createElement('div');
        formCard.className = 'settings-card';
        var html = '';
        for (var i = 0; i < _MCP_FIELDS.length; i++) {
            html += _mcpFieldHtml(_MCP_FIELDS[i]);
        }
        html += '<div class="settings-form-hint">' +
                    '配置在 MCP 连接启动时读取；修改后需在客户端重连（重启）MCP 连接才生效。' +
                '</div>' +
                '<div class="settings-save-hint">' +
                    '<button class="btn btn-primary btn-sm" id="settings-mcp-save">保存 MCP 设置</button>' +
                '</div>';
        formCard.innerHTML = html;
        section.appendChild(formCard);

        // ── 客户端配置片段卡（复制即用）──
        var snippetCard = document.createElement('div');
        snippetCard.className = 'settings-card';
        snippetCard.innerHTML =
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">客户端配置片段</label>' +
                '<select class="settings-form-select" id="settings-mcp-client-select">' +
                    '<option>加载中...</option>' +
                '</select>' +
                '<div class="settings-form-hint" id="settings-mcp-client-file"></div>' +
                '<textarea class="settings-mcp-textarea" id="settings-mcp-snippet" rows="9" readonly></textarea>' +
                '<div class="settings-form-hint" id="settings-mcp-snippet-note"></div>' +
            '</div>' +
            '<div class="settings-actions">' +
                '<button class="btn btn-secondary btn-sm" id="settings-mcp-copy-snippet">复制配置片段</button>' +
            '</div>';
        section.appendChild(snippetCard);

        parent.appendChild(section);

        // ── 事件绑定 ──
        var saveBtn = NS.$('settings-mcp-save');
        if (saveBtn) saveBtn.addEventListener('click', _handleSaveMcp);

        var refreshBtn = NS.$('settings-mcp-refresh');
        if (refreshBtn) {
            refreshBtn.addEventListener('click', function () {
                RamariaToast.show('info', '正在刷新 MCP 状态...');
                _refreshMcpInfo();
            });
        }

        var copyDbBtn = NS.$('settings-mcp-copy-db-path');
        if (copyDbBtn) copyDbBtn.addEventListener('click', _handleCopyMcpDbPath);

        var copySnippetBtn = NS.$('settings-mcp-copy-snippet');
        if (copySnippetBtn) copySnippetBtn.addEventListener('click', _handleCopyMcpSnippet);

        var selectEl = NS.$('settings-mcp-client-select');
        if (selectEl) {
            selectEl.addEventListener('change', function () {
                var idx = parseInt(selectEl.value, 10);
                if (!isNaN(idx)) {
                    state.mcpClientIndex = idx;
                    _renderMcpSnippetPicker();
                }
            });
        }

        // 白名单字段的「全部人格可见」开关联动（按元数据类型定位，不硬编码索引）
        for (var k = 0; k < _MCP_FIELDS.length; k++) {
            if (_MCP_FIELDS[k].type !== 'string-list') continue;
            var allBox = NS.$(_mcpFieldId(_MCP_FIELDS[k]) + '-all');
            if (allBox) allBox.addEventListener('change', _handleMcpAllPersonasToggle);

            // uid 输入即视为自定义：移除"默认值浅色"提示态
            var uidEl = NS.$(_mcpFieldId(_MCP_FIELDS[k]) + '-uids');
            if (uidEl) {
                uidEl.addEventListener('input', (function (el) {
                    return function () { el.classList.remove('is-default'); };
                })(uidEl));
            }
        }

        // 数值输入框：输入时同步"默认值浅色字符"状态
        for (var j = 0; j < _MCP_FIELDS.length; j++) {
            (function (field) {
                if (field.type !== 'number') return;
                var input = NS.$(_mcpFieldId(field));
                if (input) {
                    input.addEventListener('input', function () {
                        NS.syncAdvancedInputDefault(input, field);
                    });
                }
            })(_MCP_FIELDS[j]);
        }
    }

    /**
     * 渲染一个 MCP 面板字段的 HTML（由元数据驱动，与回显/收集共用同一 id 规则）。
     */
    function _mcpFieldHtml(f) {
        var fid = _mcpFieldId(f);

        if (f.type === 'bool') {
            return '<div class="settings-form-group">' +
                '<label class="settings-form-label">' +
                    '<input type="checkbox" id="' + fid + '"' + (f.def ? ' checked' : '') + ' /> ' + f.label +
                '</label>' +
                '<div class="settings-form-hint">' + f.hint + '（默认：' + (f.def ? '开' : '关') + '）</div>' +
            '</div>';
        }

        if (f.type === 'number') {
            return '<div class="settings-form-group">' +
                '<label class="settings-form-label">' + f.label + '</label>' +
                '<input class="settings-form-input is-default" id="' + fid + '" type="number" value="' + f.def + '"' +
                    (f.min !== undefined ? ' min="' + f.min + '"' : '') +
                    (f.max !== undefined ? ' max="' + f.max + '"' : '') +
                ' />' +
                '<div class="settings-form-hint">' + f.hint + '（默认：' + f.def + '）</div>' +
            '</div>';
        }

        // string-list：全部人格开关 + uid 列表输入（预留，元数据驱动）
        return '<div class="settings-form-group">' +
            '<label class="settings-form-label">' + f.label + '</label>' +
            '<div class="settings-form-whitelist">' +
                '<label class="settings-form-inline-label">' +
                    '<input type="checkbox" id="' + fid + '-all" checked /> 全部人格可见' +
                '</label>' +
            '</div>' +
            '<input class="settings-form-input is-default" id="' + fid + '-uids" type="text" ' +
                'placeholder="rama-0001, char-0002" disabled />' +
            '<div class="settings-form-hint">' + f.hint + '</div>' +
        '</div>';
    }

    /**
     * 回显 MCP 配置（进入设置页时调用）。
     *
     * 说明:
     * - 白名单语义对齐服务端：`['*']` 或空数组均视为"全部人格可见"；
     * - 数字输入框同步"默认值浅色字符"状态。
     */
    function _fillMcpForm(config) {
        if (!config || !config.mcp) return;
        var mcp = config.mcp;

        for (var i = 0; i < _MCP_FIELDS.length; i++) {
            var f = _MCP_FIELDS[i];
            var key = f.path[0];
            var fid = _mcpFieldId(f);

            if (f.type === 'bool') {
                var box = NS.$(fid);
                if (box && typeof mcp[key] === 'boolean') box.checked = mcp[key];
            } else if (f.type === 'number') {
                var input = NS.$(fid);
                if (input && typeof mcp[key] === 'number') {
                    input.value = mcp[key];
                    NS.syncAdvancedInputDefault(input, f);
                }
            }
        }

        // 白名单
        var personas = Array.isArray(mcp.allowed_personas) ? mcp.allowed_personas : [];
        var all = personas.length === 0 || personas.indexOf('*') !== -1;
        var allBox = NS.$('settings-mcp-allowed-personas-all');
        var uidInput = NS.$('settings-mcp-allowed-personas-uids');
        if (allBox) allBox.checked = all;
        if (uidInput) {
            uidInput.disabled = all;
            uidInput.value = all ? '' : personas.join(', ');
            uidInput.classList.toggle('is-default', all || personas.length === 0);
        }
    }

    /**
     * 「全部人格可见」开关联动：勾选时禁用并清空 uid 输入框。
     */
    function _handleMcpAllPersonasToggle() {
        var allBox = NS.$('settings-mcp-allowed-personas-all');
        var uidInput = NS.$('settings-mcp-allowed-personas-uids');
        if (!uidInput) return;
        var all = !!(allBox && allBox.checked);
        uidInput.disabled = all;
        if (all) uidInput.value = '';
    }

    /**
     * 解析人格 uid 列表输入（逗号/中文逗号/空白分隔，去空去重）。
     */
    function _parseUidList(text) {
        if (!text) return [];
        var parts = String(text).split(/[,，\s]+/);
        var out = [];
        for (var i = 0; i < parts.length; i++) {
            var uid = parts[i].trim();
            if (uid && out.indexOf(uid) === -1) out.push(uid);
        }
        return out;
    }

    /**
     * 从面板收集 MCP 配置。
     *
     * 返回:
     * - `{ mcp: {...} }`: 收集成功（可写回完整配置）；
     * - `{ error: '...' }`: 校验失败（数值越界 / 白名单为空），调用方提示后中止保存。
     */
    function _collectMcpConfig() {
        var mcp = {};

        for (var i = 0; i < _MCP_FIELDS.length; i++) {
            var f = _MCP_FIELDS[i];
            var key = f.path[0];
            var fid = _mcpFieldId(f);

            if (f.type === 'bool') {
                var box = NS.$(fid);
                mcp[key] = !!(box && box.checked);
            } else if (f.type === 'number') {
                var input = NS.$(fid);
                var raw = input ? input.value.trim() : '';
                var value = parseInt(raw, 10);
                if (isNaN(value)) {
                    return { error: f.label + ' 不是有效数字' };
                }
                if (f.min !== undefined && value < f.min) {
                    return { error: f.label + ' 不能小于 ' + f.min };
                }
                if (f.max !== undefined && value > f.max) {
                    return { error: f.label + ' 不能大于 ' + f.max };
                }
                mcp[key] = value;
            } else if (f.type === 'string-list') {
                var allBox = NS.$(fid + '-all');
                var uidInput = NS.$(fid + '-uids');
                var all = !!(allBox && allBox.checked);
                var uids = _parseUidList(uidInput ? uidInput.value : '');
                if (all) {
                    mcp[key] = ['*'];
                } else if (uids.length === 0) {
                    // 不写空数组：服务端把空列表按 ["*"]（全部可见）处理，与"收紧白名单"的
                    // 用户意图相反，故要求显式输入至少一个 uid
                    return { error: '请至少输入一个人格 uid，或勾选「全部人格可见」' };
                } else {
                    mcp[key] = uids;
                }
            }
        }

        return { mcp: mcp };
    }

    /**
     * 保存 MCP 接入配置（统一写入口双写）。
     */
    async function _handleSaveMcp() {
        try {
            if (!state.fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }

            var collected = _collectMcpConfig();
            if (collected.error) {
                RamariaToast.show('warning', collected.error);
                return;
            }

            var cfg = JSON.parse(JSON.stringify(state.fullConfig));
            cfg.mcp = collected.mcp;

            var result = await RamariaApi.config.updateFull(cfg);
            if (!NS.handleUpdateFullResult(result, 'MCP 接入设置已保存（客户端重连后生效）')) {
                throw new Error('配置双写均失败');
            }
            state.fullConfig = cfg;

            // 保存后刷新运行状态与配置片段（启用开关 / 库路径展示随之更新）
            await _refreshMcpInfo();
        } catch (err) {
            RamariaToast.show('error', '保存失败', err.message || '未知错误');
        }
    }

    /**
     * 刷新 MCP 接入信息（运行状态 + 配置片段素材）。
     *
     * 说明:
     * - 查询失败按空处理并提示"-"，不阻塞面板其余操作（保存仍可用）。
     */
    async function _refreshMcpInfo() {
        try {
            state.mcpInfo = await RamariaApi.mcp.getInfo();
        } catch (err) {
            console.error('[SettingsView] 查询 MCP 接入信息失败:', err);
            state.mcpInfo = null;
        }
        _fillMcpStatus(state.mcpInfo);
    }

    /**
     * 渲染 MCP 运行状态与配置片段。
     */
    function _fillMcpStatus(info) {
        var statusEl = NS.$('settings-mcp-status');
        var activityEl = NS.$('settings-mcp-activity');
        var activeEl = NS.$('settings-mcp-active-sessions');
        var dbPathEl = NS.$('settings-mcp-db-path');

        if (!info) {
            if (statusEl) statusEl.textContent = '查询失败';
            if (activityEl) activityEl.textContent = '-';
            if (activeEl) activeEl.textContent = '-';
            if (dbPathEl) dbPathEl.textContent = '-';
            state.mcpSnippets = [];
            _renderMcpSnippetPicker();
            return;
        }

        if (statusEl) {
            statusEl.textContent = info.enabled ? '✓ 已开启' : '未开启（工具不可用）';
        }
        if (activityEl) {
            activityEl.textContent = info.lastActivityMs
                ? RamariaFormat.relativeTime(info.lastActivityMs)
                : '暂无外部活动';
        }
        if (activeEl) {
            activeEl.textContent = (info.activeSessions || 0) + ' 个';
        }
        if (dbPathEl) {
            dbPathEl.textContent = info.dbPath || '-';
            dbPathEl.title = info.dbPath || '';
        }

        state.mcpSnippets = buildMcpClientSnippets(info);
        _renderMcpSnippetPicker();
    }

    /**
     * 重建客户端片段选择器与文本框内容。
     */
    function _renderMcpSnippetPicker() {
        var select = NS.$('settings-mcp-client-select');
        var fileEl = NS.$('settings-mcp-client-file');
        var textarea = NS.$('settings-mcp-snippet');
        var noteEl = NS.$('settings-mcp-snippet-note');
        if (!select) return;

        if (state.mcpSnippets.length === 0) {
            select.innerHTML = '<option>（未获取到配置信息）</option>';
            if (fileEl) fileEl.textContent = '';
            if (textarea) textarea.value = '';
            if (noteEl) noteEl.textContent = '';
            return;
        }

        select.innerHTML = '';
        for (var i = 0; i < state.mcpSnippets.length; i++) {
            var opt = document.createElement('option');
            opt.value = String(i);
            opt.textContent = state.mcpSnippets[i].title;
            select.appendChild(opt);
        }
        if (state.mcpClientIndex >= state.mcpSnippets.length) state.mcpClientIndex = 0;
        select.value = String(state.mcpClientIndex);

        var snippet = state.mcpSnippets[state.mcpClientIndex];
        // fileHint 自带完整表述（通用片段为通用说明，dsh 为具体 patch 位置）
        var hint = snippet.fileHint;
        if (state.mcpInfo && state.mcpInfo.commandIsBundled === false) {
            // 未探测到应用同目录的 CLI：提示 PATH 依赖，避免"复制后命令找不到"
            hint += '；未探测到应用同目录的 ramaria 命令，请确保其在 PATH 中或改用绝对路径';
        }
        if (fileEl) fileEl.textContent = hint;
        if (textarea) textarea.value = snippet.content;
        if (noteEl) noteEl.textContent = snippet.note || '';
    }

    async function _handleCopyMcpDbPath() {
        var dbPath = state.mcpInfo && state.mcpInfo.dbPath ? state.mcpInfo.dbPath : '';
        if (!dbPath) {
            RamariaToast.show('warning', '数据库路径尚未加载');
            return;
        }
        var ok = await NS.copyText(dbPath);
        if (ok) {
            RamariaToast.show('success', '已复制数据库路径');
        } else {
            RamariaToast.show('warning', '复制失败', '请手动从状态行选中复制');
        }
    }

    async function _handleCopyMcpSnippet() {
        if (state.mcpSnippets.length === 0) {
            RamariaToast.show('warning', '配置片段尚未加载');
            return;
        }
        var snippet = state.mcpSnippets[state.mcpClientIndex];
        if (!snippet) return;
        var ok = await NS.copyText(snippet.content);
        if (ok) {
            RamariaToast.show('success', snippet.title + ' 配置片段已复制');
        } else {
            RamariaToast.show('warning', '复制失败', '请手动选中文本框内容复制');
        }
    }

    /**
     * 生成 MCP 挂载配置片段（纯函数，面板展示与回归测试共用）。
     *
     * 参数:
     * - `info`: `{ command, dbPath }`（get_mcp_info 返回值的子集）；任一缺失返回空数组。
     *
     * 返回:
     * - 数组，每项 `{ key, title, format, fileHint, content, note }`：
     *   - `format`: `json`（通用 mcpServers）| `yaml`（DeepSeek Harness 插件 patch）；
     *   - `content`: 可直接粘贴的片段文本；
     *   - `fileHint`: 放置位置提示；`note`: 补充说明。
     *
     * 说明:
     * - 通用片段只保留最小公共字段（command + args）：覆盖 CodeBuddy / Trae 等
     *   多数客户端的 `mcpServers` 结构（Windows 路径由 JSON.stringify 正确转义）；
     * - DeepSeek Harness 为插件化配置（Cordis YAML patch），载体与 JSON 不同，
     *   单独一段；YAML 内路径用单引号包裹（反斜杠不转义，可直接粘贴）；
     * - `--db` 是 CLI 全局参数，置于 `mcp serve` 子命令之前（与 CLI 解析口径一致）。
     */
    function buildMcpClientSnippets(info) {
        if (!info || !info.command || !info.dbPath) return [];

        var args = ['--db', info.dbPath, 'mcp', 'serve'];

        return [
            {
                key: 'generic',
                title: '通用配置（JSON）',
                format: 'json',
                fileHint: '在客户端的 MCP 配置中粘贴使用',
                content: JSON.stringify(
                    { mcpServers: { ramaria: { command: info.command, args: args } } },
                    null,
                    2
                ),
                note: '如面板要求选择传输类型，选 stdio',
            },
            {
                key: 'dsh',
                title: 'DeepSeek Harness（dsh）',
                format: 'yaml',
                fileHint: '合并进 $DSH_HOME/cordis.patch.yml（或 profiles/<名称>/cordis.patch.yml）的 patch 列表，不要覆盖已有内容',
                content: _buildDshPatch(info.command, args),
                note: '工具以 mcp__ramaria__<工具名> 暴露',
            },
        ];
    }

    /**
     * 生成 DeepSeek Harness 的 MCP 客户端插件 patch（Cordis YAML）。
     *
     * 参数:
     * - `command`: MCP 服务端可执行文件路径。
     * - `args`: 启动参数（`--db <路径> mcp serve`）。
     *
     * 返回:
     * - 可直接合并进 patch 文件的 `insert` 片段文本。
     *
     * 说明:
     * - 一个插件实例对应一个 MCP server（`@deepseek-ai/dsh-mcp-client`）；
     * - 字符串统一用单引号包裹并按 YAML 规则转义（内部单引号写成两个），
     *   Windows 路径中的反斜杠保持原样、不被解释为转义序列。
     */
    function _buildDshPatch(command, args) {
        var quotedArgs = [];
        for (var i = 0; i < args.length; i++) {
            quotedArgs.push(_yamlQuote(args[i]));
        }
        return '- insert:\n' +
            '    - id: mcp-ramaria\n' +
            "      name: '@deepseek-ai/dsh-mcp-client'\n" +
            '      config:\n' +
            '        serverName: ramaria\n' +
            '        transport: stdio\n' +
            '        command: ' + _yamlQuote(command) + '\n' +
            '        args: [' + quotedArgs.join(', ') + ']\n';
    }

    /**
     * YAML 单引号字符串（内部单引号以两个单引号转义）。
     */
    function _yamlQuote(text) {
        return "'" + String(text).replace(/'/g, "''") + "'";
    }

 // =========================================================
 // 挂载面
 // =========================================================

    NS.parts.mcp = {
        /** 渲染 MCP 接入目录项（状态卡 / 配置卡 / 片段卡） */
        render: _renderMcpSection,
        /** 回显 MCP 配置（含白名单全可见语义） */
        fill: _fillMcpForm,
        /** 刷新 MCP 接入信息（运行状态 + 配置片段素材） */
        refreshInfo: _refreshMcpInfo,
        /** 面板字段元数据（默认值一致性回归读取） */
        fields: _MCP_FIELDS,
        /** 生成 MCP 挂载配置片段（纯函数，供回归测试直接驱动） */
        buildClientSnippets: buildMcpClientSnippets,
    };
})(window.RamariaSettingsNS);
