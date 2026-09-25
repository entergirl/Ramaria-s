/**
 * js/views/settings.js — Ramaria 设置页视图
 *
 * 职责:
 * - 后端配置（Provider / Base URL / Model ID / API Key）
 * - 隐私设置（隐私确认状态查看 / 记忆注入开关）
 * - MCP 接入（外部客户端挂载开关 / 白名单 / 运行状态 / 配置片段复制）
 * - 数据管理（导出 JSON / 导出 Markdown / 重建索引）
 * - 诊断与更新（检查更新 / 导出诊断信息）
 * - 关于信息（版本号 / 许可证）
 *
 * 设计特点:
 * - 注册 Router enter/leave 钩子
 * - enter 时加载当前配置并渲染表单
 * - 每个配置项独立保存（点击对应保存按钮），不自动保存
 * - API Key 遮蔽显示，eye toggle 切换可见
 * - 重建索引和导出操作带确认弹窗
 * - 导出使用 Tauri dialog.save 选择路径
 *
 * 依赖:
 * - RamariaApi / RamariaStore / RamariaRouter
 * - RamariaToast / RamariaModal
 * - TauriBridge
 * - CSS: css/views/settings.css
 */

var RamariaSettingsView = (function () {
    'use strict';

 // =========================================================
 // 内部状态
 // =========================================================

    var _unregisterFns = [];
    var _unsubs = [];

 /** 当前后端配置缓存 */
    var _backendConfig = null;
    /** 当前隐私状态 */
    var _privacyStatus = null;
    /** MCP 接入信息缓存（get_mcp_info 结果：库路径 / CLI 命令 / 活动统计） */
    var _mcpInfo = null;
    /** MCP 客户端配置片段（随 _mcpInfo 刷新重建） */
    var _mcpSnippets = [];
    /** 当前选中的客户端片段索引（客户端选择器） */
    var _mcpClientIndex = 0;

 /**
  * 本次启动（会话）是否启用调试。
  * 决定「高级设置」页签是否可见；由 app.js 在启动时读取 `debug_enabled` 设置后
  * 写入 Store.debugEnabled，本视图仅在渲染时读取，不实时跟随开关改动（重启生效）。
  */
    var _debugEnabled = false;

 // =========================================================
 // DOM 快捷查询
 // =========================================================

    function $(id) { return document.getElementById(id); }

 /**
 * 读取当前启动的调试模式。
 *
 * 优先取 Store.debugEnabled（app.js 启动时写入）；缺省时回退解析
 * Store.settings（防御启动竞态）。
 */
    function _currentDebugEnabled() {
        var cached = RamariaStore.get('debugEnabled');
        if (typeof cached === 'boolean') return cached;
        var list = RamariaStore.get('settings');
        if (list && Array.isArray(list)) {
            for (var i = 0; i < list.length; i++) {
                if (list[i] && list[i].key === 'debug_enabled') {
                    var v = list[i].value;
                    return v === true || v === 'true' || v === '1';
                }
            }
        }
        return false;
    }

 // =========================================================
 // 渲染
 // =========================================================

    function render() {
        var viewEl = $('view-settings');
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
        _debugEnabled = _currentDebugEnabled();
        var tabs = document.createElement('div');
        tabs.className = 'settings-tabs';
        tabs.innerHTML =
            '<button class="settings-tab-btn active" data-tab="basic">基础设置</button>' +
            (_debugEnabled ? '<button class="settings-tab-btn" data-tab="advanced">高级设置</button>' : '');
        scroll.appendChild(tabs);

        var basicPane = document.createElement('div');
        basicPane.className = 'settings-tab-pane';
        basicPane.id = 'settings-pane-basic';
        scroll.appendChild(basicPane);

        var advancedPane = document.createElement('div');
        advancedPane.className = 'settings-tab-pane hidden';
        advancedPane.id = 'settings-pane-advanced';
        scroll.appendChild(advancedPane);

 // ── 基础设置（面向日常用户）──
        _renderBackendSection(basicPane);
        _renderEmbeddingSection(basicPane);
        _renderMemoryInjectionSection(basicPane);
        _renderSessionSection(basicPane);
        _renderPrivacySection(basicPane);
        _renderDataDirSection(basicPane);
        _renderMcpSection(basicPane);
        _renderDataSection(basicPane);
        _renderDiagnosticsSection(basicPane);
        _renderAboutSection(basicPane);

 // ── 高级设置（面向进阶用户与排障）──
        _renderAdvancedSection(advancedPane);

 // ── Tab 切换绑定 ──
        var tabBtns = tabs.querySelectorAll('.settings-tab-btn');
        for (var i = 0; i < tabBtns.length; i++) {
            tabBtns[i].addEventListener('click', (function (btn) {
                return function () { _switchTab(btn.dataset.tab, tabBtns); };
            })(tabBtns[i]));
        }
    }

    /**
     * 切换基础/高级 Tab（v1.4 M6）。
     */
    function _switchTab(tab, btns) {
        for (var i = 0; i < btns.length; i++) {
            btns[i].classList.toggle('active', btns[i].dataset.tab === tab);
        }
        var basic = $('settings-pane-basic');
        var advanced = $('settings-pane-advanced');
        if (basic) basic.classList.toggle('hidden', tab !== 'basic');
        if (advanced) advanced.classList.toggle('hidden', tab !== 'advanced');
    }

 // =========================================================
 // 后端配置区块
 // =========================================================

    function _renderBackendSection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">🔌 后端配置</div>' +
            '<div class="settings-section-desc">配置 LLM 后端连接参数，修改后需点击保存。</div>';

        var card = document.createElement('div');
        card.className = 'settings-card';
        card.id = 'settings-backend-card';

        card.innerHTML =
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">Provider</label>' +
                '<select class="settings-form-select" id="settings-provider">' +
                    '<option value="lm_studio">LM Studio（本地）</option>' +
                    '<option value="deepseek">DeepSeek（线上）</option>' +
                    '<option value="openai">OpenAI（线上）</option>' +
                '</select>' +
            '</div>' +
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">Base URL</label>' +
                '<input class="settings-form-input" id="settings-base-url" type="text" ' +
                    'placeholder="https://api.example.com/v1" />' +
                '<div class="settings-form-hint">如果使用代理或自定义端点，可修改此地址</div>' +
            '</div>' +
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">Model ID（可选）</label>' +
                '<input class="settings-form-input" id="settings-model-id" type="text" ' +
                    'placeholder="留空使用默认模型" />' +
            '</div>' +
            // API Key 字段组用 <form> 承载：浏览器规范要求密码输入框位于表单内（否则控制台告警）；
            // 无 action / 无提交按钮，提交事件在 _bindBackendEvents 统一拦截
            '<form class="settings-form-group" id="settings-api-key-group" autocomplete="off">' +
                '<label class="settings-form-label">API Key</label>' +
                // autocomplete="new-password"：语义为"填写新密钥"（Chromium 官方建议值，
                // 缺省该属性会触发 [DOM] Input elements should have autocomplete 告警）；
                // 同时让密码管理器不自动填充旧凭据
                '<input class="settings-form-input" id="settings-api-key" type="password" ' +
                    'autocomplete="new-password" placeholder="填入新 key 以更换，留空保持不变" />' +
                '<div class="settings-form-hint">密钥存储于系统凭证管理器。当前 key：<span id="settings-api-key-hint" class="font-mono">加载中...</span></div>' +
            '</form>' +
            '<div class="settings-save-hint">' +
                '<button class="btn btn-primary btn-sm" id="settings-save-backend">保存后端配置</button>' +
            '</div>';

        section.appendChild(card);
        parent.appendChild(section);

 // 事件绑定
        _bindBackendEvents();
    }

    function _bindBackendEvents() {
// Provider 变化时调整 API Key 可见性和默认 URL
        var providerSelect = $('settings-provider');
        var apiKeyGroup = $('settings-api-key-group');
        var baseUrlInput = $('settings-base-url');

// API Key 所在 form：仅用于满足"密码字段需在表单内"的浏览器规范；
// 拦截默认提交，避免输入框内回车触发页面跳转（保存仍需点「保存后端配置」）
        if (apiKeyGroup && apiKeyGroup.tagName === 'FORM') {
            apiKeyGroup.addEventListener('submit', function (e) {
                e.preventDefault();
            });
        }

        if (providerSelect && apiKeyGroup && baseUrlInput) {
            providerSelect.addEventListener('change', function () {
                var isLocal = this.value === 'lm_studio';
                apiKeyGroup.classList.toggle('hidden', isLocal);

 // 自动填充默认 URL
                if (!baseUrlInput.value || baseUrlInput.value === _getDefaultUrl(_backendConfig ? _backendConfig.provider : '')) {
                    baseUrlInput.value = _getDefaultUrl(this.value);
                }
            });
        }

 // 保存按钮
        var saveBtn = $('settings-save-backend');
        if (saveBtn) {
            saveBtn.addEventListener('click', _handleSaveBackend);
        }
    }

    function _getDefaultUrl(provider) {
        if (provider === 'lm_studio') return 'http://localhost:1234/v1';
        if (provider === 'deepseek') return 'https://api.deepseek.com/v1';
        if (provider === 'openai') return 'https://api.openai.com/v1';
        return '';
    }

    function _fillBackendForm(config) {
        if (!config) return;

 // Tauri 2 将 Rust snake_case 字段序列化为 camelCase
        var providerEl = $('settings-provider');
        var baseUrlEl = $('settings-base-url');
        var modelIdEl = $('settings-model-id');
        var apiKeyHint = $('settings-api-key-hint');
        var apiKeyGroup = $('settings-api-key-group');

 // provider 值匹配下拉选项（as_str 返回 snake_case：lm_studio / deepseek / openai）
        if (providerEl) providerEl.value = config.provider || 'lm_studio';
        if (baseUrlEl) baseUrlEl.value = config.baseUrl || _getDefaultUrl(config.provider);
        if (modelIdEl) modelIdEl.value = config.modelId || '';

 // 遮罩 key 显示在 hint 中，输入框留给用户填新 key
        if (apiKeyHint) apiKeyHint.textContent = config.apiKeyMasked || '未配置';

 // 本地 provider 隐藏 API Key 输入组
        var isLocal = (config.provider === 'lm_studio');
        if (apiKeyGroup) apiKeyGroup.classList.toggle('hidden', isLocal);
    }

    async function _handleSaveBackend() {
        var provider = ($('settings-provider') || {}).value;
        var baseUrl = ($('settings-base-url') || {}).value;
        var modelId = ($('settings-model-id') || {}).value;
        var apiKey = ($('settings-api-key') || {}).value;

        if (!provider || !baseUrl) {
            RamariaToast.show('warning', '请填写 Provider 和 Base URL');
            return;
        }

 // 线上 API Key 检查
        if (provider !== 'lm_studio' && (!apiKey || apiKey.trim().length < 5)) {
            RamariaToast.show('warning', '线上后端需要提供 API Key');
            return;
        }

        var saveBtn = $('settings-save-backend');
        if (saveBtn) {
            saveBtn.disabled = true;
            saveBtn.textContent = '保存中...';
        }

        try {
            await RamariaApi.config.updateBackend(provider, modelId || '', baseUrl, apiKey || '');
            RamariaToast.show('success', '后端配置已保存');

 // 刷新本地缓存
            _backendConfig = await RamariaApi.config.getBackend();
            RamariaStore.set('backendConfig', _backendConfig);

 // 清空 API Key 输入
            var keyInput = $('settings-api-key');
            if (keyInput) keyInput.value = '';

        } catch (err) {
            console.error('[SettingsView] 保存后端配置失败:', err);
            RamariaToast.show('error', '保存失败', err.message || '未知错误');
        } finally {
            if (saveBtn) {
                saveBtn.disabled = false;
                saveBtn.textContent = '保存后端配置';
            }
        }
    }

 // =========================================================
 // 嵌入模型配置区块
 // =========================================================

    function _renderEmbeddingSection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">🧲 嵌入模型</div>' +
            '<div class="settings-section-desc">配置本地嵌入模型，用于记忆语义检索（向量搜索）。</div>';

        var card = document.createElement('div');
        card.className = 'settings-card';
        card.id = 'settings-embedding-card';
        card.innerHTML =
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">模型文件夹路径</label>' +
                '<input class="settings-form-input" id="settings-embedding-path" type="text" ' +
                    'placeholder="D:/models/bge-small-zh-v1.5" />' +
                '<div class="settings-form-hint">' +
                    '推荐模型：<code class="settings-code-inline">BAAI/bge-small-zh-v1.5</code>（约 100MB）。' +
                    '留空则使用 BM25 + 图谱降级模式，不进行向量检索。' +
                '</div>' +
            '</div>' +
            '<div class="settings-form-group hidden" id="settings-embedding-status-group">' +
                '<div class="settings-row">' +
                    '<div>' +
                        '<div class="settings-row-label">当前状态</div>' +
                        '<div class="settings-row-meta" id="settings-embedding-status">加载中...</div>' +
                    '</div>' +
                    '<span class="settings-row-value" id="settings-embedding-valid-badge">-</span>' +
                '</div>' +
            '</div>' +
            '<div class="settings-save-hint">' +
                '<button class="btn btn-secondary btn-sm" id="settings-validate-embedding">校验路径</button>' +
                '<button class="btn btn-primary btn-sm" id="settings-save-embedding">保存</button>' +
            '</div>';

        section.appendChild(card);
        parent.appendChild(section);

 // 绑定事件
        var validateBtn = $('settings-validate-embedding');
        var saveBtn = $('settings-save-embedding');
        if (validateBtn) validateBtn.addEventListener('click', _handleValidateEmbedding);
        if (saveBtn) saveBtn.addEventListener('click', _handleSaveEmbedding);
    }

 // =========================================================
 // 记忆注入开关区块（v1.4 M6 基础设置）
 // =========================================================

    function _renderMemoryInjectionSection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">🧠 记忆注入</div>' +
            '<div class="settings-section-desc">控制线上 LLM 后端是否接收记忆上下文（L1/L2/L3 摘要与检索结果）。</div>';

        var card = document.createElement('div');
        card.className = 'settings-card';
        card.innerHTML =
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">' +
                    '<input type="checkbox" id="settings-memory-injection" /> 允许线上后端注入记忆上下文' +
                '</label>' +
                '<div class="settings-form-hint">' +
                    '关闭后线上 provider 的请求不携带记忆上下文（本地 LM Studio 不受影响）。' +
                    '默认开启；开启线上注入前需完成隐私确认。' +
                '</div>' +
            '</div>' +
            '<div class="settings-save-hint">' +
                '<button class="btn btn-primary btn-sm" id="settings-save-memory-injection">保存记忆注入设置</button>' +
            '</div>';

        section.appendChild(card);
        parent.appendChild(section);

        var btn = $('settings-save-memory-injection');
        if (btn) btn.addEventListener('click', _handleSaveMemoryInjection);
    }

    /**
     * 回显记忆注入开关（online_memory_injection）。
     */
    function _fillMemoryInjectionForm(config) {
        if (!config || !config.backend) return;
        var box = $('settings-memory-injection');
        if (box) box.checked = !!config.backend.online_memory_injection;
    }

    /**
     * 保存记忆注入开关：完整配置更新（统一写入口双写）。
     */
    async function _handleSaveMemoryInjection() {
        try {
            if (!_fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }
            var box = $('settings-memory-injection');
            if (!box) return;

            var cfg = JSON.parse(JSON.stringify(_fullConfig));
            cfg.backend.online_memory_injection = box.checked;

            var result = await RamariaApi.config.updateFull(cfg);
            if (!_handleUpdateFullResult(result, '记忆注入设置已保存')) {
                throw new Error('配置双写均失败');
            }
            _fullConfig = cfg;
        } catch (err) {
            RamariaToast.show('error', '保存失败', err.message || '未知错误');
        }
    }

 // =========================================================
 // 数据目录区块（v1.4 M6 基础设置）
 // =========================================================

    function _renderDataDirSection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">📂 数据目录</div>' +
            '<div class="settings-section-desc">查看与修改 Ramaria 数据目录（数据库/索引/日志存储位置）。</div>';

        var card = document.createElement('div');
        card.className = 'settings-card';
        card.innerHTML =
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">数据目录</label>' +
                '<input class="settings-form-input" id="settings-data-dir" type="text" placeholder="%APPDATA%\\Ramaria\\data" />' +
                '<div class="settings-form-hint">修改后需重启应用生效。留空使用系统默认位置。</div>' +
            '</div>' +
            '<div class="settings-save-hint">' +
                '<button class="btn btn-primary btn-sm" id="settings-save-data-dir">保存数据目录</button>' +
            '</div>';

        section.appendChild(card);
        parent.appendChild(section);

        var btn = $('settings-save-data-dir');
        if (btn) btn.addEventListener('click', _handleSaveDataDir);
    }

    /**
     * 回显数据目录（paths.data_dir）。
     */
    function _fillDataDirForm(config) {
        if (!config || !config.paths) return;
        var input = $('settings-data-dir');
        if (input && typeof config.paths.data_dir === 'string') {
            input.value = config.paths.data_dir;
        }
    }

    /**
     * 保存数据目录：完整配置更新（统一写入口双写）。
     */
    async function _handleSaveDataDir() {
        try {
            if (!_fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }
            var input = $('settings-data-dir');
            if (!input) return;

            var cfg = JSON.parse(JSON.stringify(_fullConfig));
            cfg.paths.data_dir = input.value.trim();

            var result = await RamariaApi.config.updateFull(cfg);
            if (!_handleUpdateFullResult(result, '数据目录已保存（重启后生效）')) {
                throw new Error('配置双写均失败');
            }
            _fullConfig = cfg;
        } catch (err) {
            RamariaToast.show('error', '保存失败', err.message || '未知错误');
        }
    }

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
            '原文块默认不出端，可用下方人格白名单与原文开关收紧可见范围。';
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
        var saveBtn = $('settings-mcp-save');
        if (saveBtn) saveBtn.addEventListener('click', _handleSaveMcp);

        var refreshBtn = $('settings-mcp-refresh');
        if (refreshBtn) {
            refreshBtn.addEventListener('click', function () {
                RamariaToast.show('info', '正在刷新 MCP 状态...');
                _refreshMcpInfo();
            });
        }

        var copyDbBtn = $('settings-mcp-copy-db-path');
        if (copyDbBtn) copyDbBtn.addEventListener('click', _handleCopyMcpDbPath);

        var copySnippetBtn = $('settings-mcp-copy-snippet');
        if (copySnippetBtn) copySnippetBtn.addEventListener('click', _handleCopyMcpSnippet);

        var selectEl = $('settings-mcp-client-select');
        if (selectEl) {
            selectEl.addEventListener('change', function () {
                var idx = parseInt(selectEl.value, 10);
                if (!isNaN(idx)) {
                    _mcpClientIndex = idx;
                    _renderMcpSnippetPicker();
                }
            });
        }

        // 白名单字段的「全部人格可见」开关联动（按元数据类型定位，不硬编码索引）
        for (var k = 0; k < _MCP_FIELDS.length; k++) {
            if (_MCP_FIELDS[k].type !== 'string-list') continue;
            var allBox = $(_mcpFieldId(_MCP_FIELDS[k]) + '-all');
            if (allBox) allBox.addEventListener('change', _handleMcpAllPersonasToggle);

            // uid 输入即视为自定义：移除"默认值浅色"提示态
            var uidEl = $(_mcpFieldId(_MCP_FIELDS[k]) + '-uids');
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
                var input = $(_mcpFieldId(field));
                if (input) {
                    input.addEventListener('input', function () {
                        _syncAdvancedInputDefault(input, field);
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
                var box = $(fid);
                if (box && typeof mcp[key] === 'boolean') box.checked = mcp[key];
            } else if (f.type === 'number') {
                var input = $(fid);
                if (input && typeof mcp[key] === 'number') {
                    input.value = mcp[key];
                    _syncAdvancedInputDefault(input, f);
                }
            }
        }

        // 白名单
        var personas = Array.isArray(mcp.allowed_personas) ? mcp.allowed_personas : [];
        var all = personas.length === 0 || personas.indexOf('*') !== -1;
        var allBox = $('settings-mcp-allowed-personas-all');
        var uidInput = $('settings-mcp-allowed-personas-uids');
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
        var allBox = $('settings-mcp-allowed-personas-all');
        var uidInput = $('settings-mcp-allowed-personas-uids');
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
                var box = $(fid);
                mcp[key] = !!(box && box.checked);
            } else if (f.type === 'number') {
                var input = $(fid);
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
                var allBox = $(fid + '-all');
                var uidInput = $(fid + '-uids');
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
            if (!_fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }

            var collected = _collectMcpConfig();
            if (collected.error) {
                RamariaToast.show('warning', collected.error);
                return;
            }

            var cfg = JSON.parse(JSON.stringify(_fullConfig));
            cfg.mcp = collected.mcp;

            var result = await RamariaApi.config.updateFull(cfg);
            if (!_handleUpdateFullResult(result, 'MCP 接入设置已保存（客户端重连后生效）')) {
                throw new Error('配置双写均失败');
            }
            _fullConfig = cfg;

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
            _mcpInfo = await RamariaApi.mcp.getInfo();
        } catch (err) {
            console.error('[SettingsView] 查询 MCP 接入信息失败:', err);
            _mcpInfo = null;
        }
        _fillMcpStatus(_mcpInfo);
    }

    /**
     * 渲染 MCP 运行状态与配置片段。
     */
    function _fillMcpStatus(info) {
        var statusEl = $('settings-mcp-status');
        var activityEl = $('settings-mcp-activity');
        var activeEl = $('settings-mcp-active-sessions');
        var dbPathEl = $('settings-mcp-db-path');

        if (!info) {
            if (statusEl) statusEl.textContent = '查询失败';
            if (activityEl) activityEl.textContent = '-';
            if (activeEl) activeEl.textContent = '-';
            if (dbPathEl) dbPathEl.textContent = '-';
            _mcpSnippets = [];
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

        _mcpSnippets = buildMcpClientSnippets(info);
        _renderMcpSnippetPicker();
    }

    /**
     * 重建客户端片段选择器与文本框内容。
     */
    function _renderMcpSnippetPicker() {
        var select = $('settings-mcp-client-select');
        var fileEl = $('settings-mcp-client-file');
        var textarea = $('settings-mcp-snippet');
        var noteEl = $('settings-mcp-snippet-note');
        if (!select) return;

        if (_mcpSnippets.length === 0) {
            select.innerHTML = '<option>（未获取到配置信息）</option>';
            if (fileEl) fileEl.textContent = '';
            if (textarea) textarea.value = '';
            if (noteEl) noteEl.textContent = '';
            return;
        }

        select.innerHTML = '';
        for (var i = 0; i < _mcpSnippets.length; i++) {
            var opt = document.createElement('option');
            opt.value = String(i);
            opt.textContent = _mcpSnippets[i].title;
            select.appendChild(opt);
        }
        if (_mcpClientIndex >= _mcpSnippets.length) _mcpClientIndex = 0;
        select.value = String(_mcpClientIndex);

        var snippet = _mcpSnippets[_mcpClientIndex];
        // fileHint 自带完整表述（通用片段为通用说明，dsh 为具体 patch 位置）
        var hint = snippet.fileHint;
        if (_mcpInfo && _mcpInfo.commandIsBundled === false) {
            // 未探测到应用同目录的 CLI：提示 PATH 依赖，避免"复制后命令找不到"
            hint += '；未探测到应用同目录的 ramaria 命令，请确保其在 PATH 中或改用绝对路径';
        }
        if (fileEl) fileEl.textContent = hint;
        if (textarea) textarea.value = snippet.content;
        if (noteEl) noteEl.textContent = snippet.note || '';
    }

    async function _handleCopyMcpDbPath() {
        var dbPath = _mcpInfo && _mcpInfo.dbPath ? _mcpInfo.dbPath : '';
        if (!dbPath) {
            RamariaToast.show('warning', '数据库路径尚未加载');
            return;
        }
        var ok = await _copyText(dbPath);
        if (ok) {
            RamariaToast.show('success', '已复制数据库路径');
        } else {
            RamariaToast.show('warning', '复制失败', '请手动从状态行选中复制');
        }
    }

    async function _handleCopyMcpSnippet() {
        if (_mcpSnippets.length === 0) {
            RamariaToast.show('warning', '配置片段尚未加载');
            return;
        }
        var snippet = _mcpSnippets[_mcpClientIndex];
        if (!snippet) return;
        var ok = await _copyText(snippet.content);
        if (ok) {
            RamariaToast.show('success', snippet.title + ' 配置片段已复制');
        } else {
            RamariaToast.show('warning', '复制失败', '请手动选中文本框内容复制');
        }
    }

    /**
     * 复制文本到剪贴板。
     *
     * 策略:
     * 1. 优先 Clipboard API（Tauri WebView2 支持）；
     * 2. 失败/不可用时回退临时 textarea + execCommand（旧内核兜底）。
     *
     * 返回:
     * - `true`: 复制成功；`false`: 两条路径均失败（调用方提示手动复制）。
     */
    async function _copyText(text) {
        if (!text) return false;

        try {
            if (typeof navigator !== 'undefined' && navigator.clipboard && navigator.clipboard.writeText) {
                await navigator.clipboard.writeText(text);
                return true;
            }
        } catch (err) {
            console.warn('[SettingsView] Clipboard API 复制失败，尝试回退方案:', err);
        }

        try {
            var ta = document.createElement('textarea');
            ta.value = text;
            ta.setAttribute('readonly', 'readonly');
            ta.style.position = 'fixed';
            ta.style.opacity = '0';
            document.body.appendChild(ta);
            ta.select();
            var ok = document.execCommand('copy');
            document.body.removeChild(ta);
            return ok;
        } catch (err) {
            console.error('[SettingsView] 复制失败:', err);
            return false;
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
// 会话区块（v1.4 M5：空闲自动保存时长滑动块）
// =========================================================

    // 完整生效配置缓存（getFullConfig 回显 + 保存时回写）
    var _fullConfig = null;

    function _renderSessionSection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">💬 会话</div>' +
            '<div class="settings-section-desc">设置会话空闲自动保存时长，修改后立即生效（无需重启）。</div>';

        var card = document.createElement('div');
        card.className = 'settings-card';
        card.id = 'settings-session-card';
        card.innerHTML =
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">空闲自动保存时长</label>' +
                '<div class="settings-row">' +
                    '<input class="settings-range" id="settings-idle-slider" type="range" min="5" max="60" step="1" value="10" />' +
                    '<span class="settings-range-value" id="settings-idle-value">10 分钟</span>' +
                '</div>' +
                '<div class="settings-form-hint">' +
                    '对话空闲超过该时长后自动保存并生成记忆摘要。滑动到最右端（60 分钟）可切换为自定义输入。' +
                '</div>' +
            '</div>' +
            '<div class="settings-form-group hidden" id="settings-idle-custom-group">' +
                '<label class="settings-form-label">自定义时长（分钟）</label>' +
                '<input class="settings-form-input" id="settings-idle-custom" type="number" min="1" placeholder="例如 90" />' +
                '<div class="settings-form-hint">自定义时长 ≥ 1 分钟，超出 60 分钟时保存为自定义值。</div>' +
            '</div>' +
            '<div class="settings-save-hint">' +
                '<button class="btn btn-primary btn-sm" id="settings-save-session">保存会话设置</button>' +
            '</div>';

        section.appendChild(card);
        parent.appendChild(section);

        // 绑定事件
        var slider = $('settings-idle-slider');
        var saveBtn = $('settings-save-session');
        if (slider) slider.addEventListener('input', _handleIdleSlider);
        if (saveBtn) saveBtn.addEventListener('click', _handleSaveSession);
    }

    /**
     * 滑动块联动：滑到尽头（60）切换自定义输入，其余显示分钟数。
     */
    function _handleIdleSlider() {
        var slider = $('settings-idle-slider');
        var valueEl = $('settings-idle-value');
        var customGroup = $('settings-idle-custom-group');
        var custom = $('settings-idle-custom');
        if (!slider || !valueEl) return;

        var v = parseInt(slider.value, 10);
        if (v >= 60) {
            valueEl.textContent = '自定义';
            if (customGroup) customGroup.classList.remove('hidden');
            if (custom) custom.focus();
        } else {
            valueEl.textContent = v + ' 分钟';
            if (customGroup) customGroup.classList.add('hidden');
        }
    }

    /**
     * 回显：5~60 落到滑动块；其余（自定义值）滑动块置尽头 + 显示自定义输入。
     */
    function _fillSessionForm(config) {
        if (!config || !config.session) return;
        var minutes = config.session.l1_idle_minutes;
        if (typeof minutes !== 'number') return;

        var slider = $('settings-idle-slider');
        var valueEl = $('settings-idle-value');
        var customGroup = $('settings-idle-custom-group');
        var custom = $('settings-idle-custom');

        if (minutes >= 5 && minutes <= 60) {
            if (slider) slider.value = minutes;
            if (valueEl) valueEl.textContent = minutes + ' 分钟';
            if (customGroup) customGroup.classList.add('hidden');
        } else {
            if (slider) slider.value = 60;
            if (valueEl) valueEl.textContent = '自定义';
            if (customGroup) customGroup.classList.remove('hidden');
            if (custom) custom.value = minutes;
        }
    }

    /**
     * 保存会话设置：读当前值 → 完整配置更新（后端双写 + 热更新阈值）。
     */
    async function _handleSaveSession() {
        try {
            if (!_fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }
            var minutes = _readIdleMinutes();
            if (minutes === null) return;

            // 深拷贝后仅改 session.l1_idle_minutes，其余字段原样回写
            var cfg = JSON.parse(JSON.stringify(_fullConfig));
            cfg.session.l1_idle_minutes = minutes;

            var result = await RamariaApi.config.updateFull(cfg);
            if (result && result.fileOk === false && result.dbOk === false) {
                throw new Error('配置双写均失败');
            }
            // 后端保存成功后已热更新运行时阈值（与空闲检测线程联动，无需重启）
            _fullConfig.session.l1_idle_minutes = minutes;
            RamariaToast.show('success', '会话设置已保存（立即生效）');
        } catch (err) {
            RamariaToast.show('error', '保存会话设置失败', err.message || '未知错误');
        }
    }

    /**
     * 读取当前选中的空闲时长（分钟）；非法输入返回 null 并提示。
     */
    function _readIdleMinutes() {
        var customGroup = $('settings-idle-custom-group');
        var custom = $('settings-idle-custom');
        var slider = $('settings-idle-slider');
        if (customGroup && !customGroup.classList.contains('hidden') && custom) {
            var v = parseInt(custom.value, 10);
            if (isNaN(v) || v < 1) {
                RamariaToast.show('warning', '请输入有效的自定义时长（≥ 1 分钟）');
                return null;
            }
            return v;
        }
        if (slider) return parseInt(slider.value, 10);
        return null;
    }

 /**
 * 填充嵌入模型配置表单。
 */
    function _fillEmbeddingForm(config) {
        var pathEl = $('settings-embedding-path');
        var statusGroup = $('settings-embedding-status-group');
        var statusEl = $('settings-embedding-status');
        var badgeEl = $('settings-embedding-valid-badge');

        if (pathEl) pathEl.value = (config && config.modelPath) || '';

        if (statusGroup && config && config.modelPath) {
            statusGroup.classList.remove('hidden');
            if (statusEl) {
                statusEl.textContent = config.valid
                    ? '嵌入模型就绪（' + (config.dimension || '?') + ' 维）'
                    : '嵌入模型路径无效或文件不完整';
            }
            if (badgeEl) {
                badgeEl.textContent = config.valid ? '✓ 可用' : '✗ 不可用';
                badgeEl.classList.toggle('text-green', config.valid);
                badgeEl.classList.toggle('text-pink', !config.valid);
            }
        } else if (statusGroup) {
            statusGroup.classList.add('hidden');
        }
    }

 /**
 * 校验嵌入模型路径。
 */
    async function _handleValidateEmbedding() {
        var pathEl = $('settings-embedding-path');
        var path = pathEl ? pathEl.value.trim() : '';
        if (!path) {
            RamariaToast.show('warning', '请先填写模型文件夹路径');
            return;
        }

 // 统一正斜杠
        if (path.indexOf('\\') !== -1) {
            path = path.replace(/\\/g, '/');
            if (pathEl) pathEl.value = path;
        }

        var validateBtn = $('settings-validate-embedding');
        if (validateBtn) {
            validateBtn.disabled = true;
            validateBtn.textContent = '校验中...';
        }

        try {
            var result = await RamariaApi.setup.validateEmbeddingModel(path);
            var statusGroup = $('settings-embedding-status-group');
            var statusEl = $('settings-embedding-status');
            var badgeEl = $('settings-embedding-valid-badge');

            if (statusGroup) statusGroup.classList.remove('hidden');

            if (result && result.valid) {
                if (statusEl) statusEl.textContent = '嵌入模型就绪（' + (result.dimension || '?') + ' 维）';
                if (badgeEl) {
                    badgeEl.textContent = '✓ 可用';
                    badgeEl.classList.add('text-green');
                    badgeEl.classList.remove('text-pink');
                }
                RamariaToast.show('success', '模型校验通过');
            } else {
                if (statusEl) statusEl.textContent = (result && result.reason) || '模型路径无效或文件不完整';
                if (badgeEl) {
                    badgeEl.textContent = '✗ 不可用';
                    badgeEl.classList.remove('text-green');
                    badgeEl.classList.add('text-pink');
                }
                RamariaToast.show('error', '校验失败', (result && result.reason) || '路径无效');
            }
        } catch (err) {
            RamariaToast.show('error', '校验失败', err.message || '未知错误');
        } finally {
            if (validateBtn) {
                validateBtn.disabled = false;
                validateBtn.textContent = '校验路径';
            }
        }
    }

 /**
 * 保存嵌入模型配置。
 */
    async function _handleSaveEmbedding() {
        var pathEl = $('settings-embedding-path');
        var path = pathEl ? pathEl.value.trim() : '';

        var saveBtn = $('settings-save-embedding');
        if (saveBtn) {
            saveBtn.disabled = true;
            saveBtn.textContent = '保存中...';
        }

        try {
            await RamariaApi.setup.saveEmbeddingModel(path);

            if (path) {
                RamariaToast.show('success', '嵌入模型配置已保存，重启后生效');
            } else {
                RamariaToast.show('info', '嵌入模型已移除，应用将进入降级模式');
            }

 // 刷新应用状态
            try {
                var newState = await RamariaApi.setup.refresh();
                if (newState) RamariaStore.set('appState', newState);
            } catch (_) { /* ignore */ }
        } catch (err) {
            RamariaToast.show('error', '保存失败', err.message || '未知错误');
        } finally {
            if (saveBtn) {
                saveBtn.disabled = false;
                saveBtn.textContent = '保存';
            }
        }
    }

 // =========================================================
 // 隐私设置区块
 // =========================================================

    function _renderPrivacySection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">🔒 隐私设置</div>' +
            '<div class="settings-section-desc">管理线上服务的隐私确认和数据控制。</div>';

        var card = document.createElement('div');
        card.className = 'settings-card';
        card.id = 'settings-privacy-card';
        card.innerHTML =
            '<div class="settings-row">' +
                '<div>' +
                    '<div class="settings-row-label">隐私确认状态</div>' +
                    '<div class="settings-row-meta">当前后端对线上服务的确认状态</div>' +
                '</div>' +
                '<span class="settings-row-value" id="settings-privacy-status">加载中...</span>' +
            '</div>' +
            '<div class="settings-row">' +
                '<div>' +
                    '<div class="settings-row-label">持久化确认</div>' +
                    '<div class="settings-row-meta">勾选后跨重启不再提示</div>' +
                '</div>' +
                '<span class="settings-row-value" id="settings-privacy-persistent">-</span>' +
            '</div>' +
            '<div class="settings-actions">' +
                '<button class="btn btn-secondary btn-sm" id="settings-confirm-privacy">确认隐私</button>' +
            '</div>';

        section.appendChild(card);
        parent.appendChild(section);

 // 绑定
        var confirmBtn = $('settings-confirm-privacy');
        if (confirmBtn) {
            confirmBtn.addEventListener('click', async function () {
                try {
                    await RamariaApi.chat.confirmPrivacy(true);
                    RamariaToast.show('success', '隐私确认已保存');
                    await _refreshPrivacyStatus();
                } catch (err) {
                    RamariaToast.show('error', '确认失败', err.message || '');
                }
            });
        }
    }

    function _fillPrivacyInfo(status) {
        var statusEl = $('settings-privacy-status');
        var persistentEl = $('settings-privacy-persistent');

        if (statusEl) {
            if (!status) {
                statusEl.textContent = '-';
            } else if (status.status === 'NotNeeded') {
                statusEl.textContent = '无需确认（本地）';
            } else if (status.status === 'Confirmed') {
                statusEl.textContent = '✓ 已确认';
            } else {
                statusEl.textContent = '⚠ 需要确认';
            }
        }

        if (persistentEl) {
            persistentEl.textContent = (status && status.persistent) ? '✓ 是' : '✗ 否';
        }

        _privacyStatus = status;
    }

    async function _refreshPrivacyStatus() {
        try {
            var status = await RamariaApi.chat.checkPrivacy();
            _fillPrivacyInfo(status);
        } catch (err) {
            console.error('[SettingsView] 查询隐私状态失败:', err);
        }
    }

 // =========================================================
 // 数据管理区块
 // =========================================================

    function _renderDataSection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">💾 数据管理</div>' +
            '<div class="settings-section-desc">导出记忆数据或重建检索索引。</div>';

        var card = document.createElement('div');
        card.className = 'settings-card';
        card.innerHTML =
            '<div class="settings-actions">' +
                '<button class="btn btn-secondary btn-sm" id="settings-export-json">导出 JSON</button>' +
                '<button class="btn btn-secondary btn-sm" id="settings-export-md">导出 Markdown</button>' +
            '</div>' +
            '<div class="settings-danger-zone mt-4">' +
                '<div class="settings-danger-title">⚠ 重建索引</div>' +
                '<div class="settings-danger-desc">' +
                    '重建全部记忆检索索引。在索引数据异常或检索结果不准确时可以执行此操作。' +
                    '重建期间无法正常对话。<br>重建不会丢失任何记忆数据。' +
                '</div>' +
                '<button class="btn btn-primary btn-sm" id="settings-rebuild-index">重建索引</button>' +
            '</div>';

        section.appendChild(card);
        parent.appendChild(section);

 // 导出 JSON
        var exportJsonBtn = $('settings-export-json');
        if (exportJsonBtn) {
            exportJsonBtn.addEventListener('click', _handleExportJson);
        }

 // 导出 Markdown
        var exportMdBtn = $('settings-export-md');
        if (exportMdBtn) {
            exportMdBtn.addEventListener('click', _handleExportMarkdown);
        }

 // 重建索引
        var rebuildBtn = $('settings-rebuild-index');
        if (rebuildBtn) {
            rebuildBtn.addEventListener('click', _handleRebuildIndex);
        }
    }

    async function _handleExportJson() {
        try {
            var path = await _pickSavePath('ramaria-export.json', 'JSON 文件');
            if (!path) return;

            RamariaToast.show('info', '正在导出...');
            var result = await RamariaApi.export.json(path);
            RamariaToast.show('success', '导出完成', result || path);
        } catch (err) {
            if (err.message && err.message.indexOf('cancel') !== -1) return;
            console.error('[SettingsView] 导出 JSON 失败:', err);
            RamariaToast.show('error', '导出失败', err.message || '未知错误');
        }
    }

    async function _handleExportMarkdown() {
        try {
            var path = await _pickSavePath('ramaria-export.md', 'Markdown 文件');
            if (!path) return;

            RamariaToast.show('info', '正在导出...');
            var result = await RamariaApi.export.markdown(path);
            RamariaToast.show('success', '导出完成', result || path);
        } catch (err) {
            if (err.message && err.message.indexOf('cancel') !== -1) return;
            console.error('[SettingsView] 导出 Markdown 失败:', err);
            RamariaToast.show('error', '导出失败', err.message || '未知错误');
        }
    }

    async function _pickSavePath(defaultName, filterName) {
 // 使用 Tauri dialog plugin 的原生保存对话框
        if (window.__TAURI__ && window.__TAURI__.dialog && window.__TAURI__.dialog.save) {
            try {
                var result = await window.__TAURI__.dialog.save({
                    defaultPath: defaultName,
                    filters: [{
                        name: filterName || '文件',
                        extensions: [defaultName.split('.').pop() || 'txt'],
                    }],
                });
 // 用户取消时 result 为 null
                return result || null;
            } catch (err) {
                console.warn('[SettingsView] 原生文件对话框失败:', err);
            }
        }

 // 尝试通过 Tauri invoke 调用
        if (TauriBridge && TauriBridge.invoke) {
            try {
                var invokeResult = await TauriBridge.invoke('save_file_dialog', {
                    default_name: defaultName,
                    filter_name: filterName,
                });
                if (invokeResult) return invokeResult;
            } catch (err) {
                console.warn('[SettingsView] 调用 save_file_dialog 失败:', err);
            }
        }

 // 最终降级：浏览器 prompt
        var path = prompt('请输入导出文件路径（例：C:\\Users\\YourName\\Desktop\\' + defaultName + ')', defaultName);
        return path;
    }

    async function _handleRebuildIndex() {
        RamariaModal.show({
            title: '确认重建索引',
            body: '<p class="settings-modal-body">' +
                  '重建索引将重新扫描全部记忆数据并构建检索索引。<br><br>' +
                  '<strong>注意：</strong>重建期间无法进行对话。此操作不会丢失数据，但可能需要几分钟时间。</p>',
            footer: '<button class="btn btn-secondary" data-action="cancel">取消</button>' +
                    '<button class="btn btn-primary" data-action="confirm">确认重建</button>',
            onAction: async function (action) {
                if (action !== 'confirm') return;

                try {
                    RamariaToast.show('info', '正在重建索引...', '', { duration: 10000 });
                    var count = await RamariaApi.index.rebuild();
                    RamariaToast.show('success', '索引重建完成', '处理了 ' + (count || '?') + ' 篇文档');

 // 刷新应用状态
                    try {
                        var newState = await RamariaApi.setup.refresh();
                        if (newState) RamariaStore.set('appState', newState);
                    } catch (_) { /* ignore */ }
                } catch (err) {
                    console.error('[SettingsView] 重建索引失败:', err);
                    RamariaToast.show('error', '重建失败', err.message || '未知错误');
                }
            },
        });
    }

 // =========================================================
 // 版本加载（页面进入时静默调用）
 // =========================================================

 /**
 * 静默加载当前版本信息。
 *
 * 行为:
 * - 调用 getVersion API（纯本地，无网络请求，不消耗 GitHub API 配额）。
 * - 更新页面上的版本显示。
 * - 不显示 toast，不改变按钮状态。
 */
    async function _loadVersion() {
        try {
            var version = await RamariaApi.diagnostics.getVersion();

            var versionEl = $('settings-current-version');
            if (versionEl) versionEl.textContent = 'v' + (version || '?');

 // 同步更新"关于"区块的版本号
            var aboutVersionEl = $('settings-about-version');
            if (aboutVersionEl) aboutVersionEl.textContent = 'v' + (version || '?');
        } catch (_) {
 // 静默忽略加载失败
        }
    }

 // =========================================================
 // 诊断与更新区块
 // =========================================================

    function _renderDiagnosticsSection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">🔧 诊断与更新</div>' +
            '<div class="settings-section-desc">检查新版本或导出诊断信息以排查问题。</div>';

 // ── 启用调试（v2.0 M7）：重启生效；开启后显示侧边栏「调试」与设置页「高级设置」──
        var debugCard = document.createElement('div');
        debugCard.className = 'settings-card';
        debugCard.innerHTML =
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">' +
                    '<input type="checkbox" id="settings-debug-enabled"' + (_debugEnabled ? ' checked' : '') + ' /> ' +
                    '启用调试（开发者模式）' +
                '</label>' +
                '<div class="settings-form-hint">' +
                    '开启并<strong>重启 Ramaria</strong> 后：① 侧边栏出现「调试」页面；② 设置页出现「高级设置」页签。' +
                    '关闭并重启则恢复默认（仅基础设置、无调试页）。' +
                '</div>' +
            '</div>';
        section.appendChild(debugCard);

        var card = document.createElement('div');
        card.className = 'settings-card';
        card.innerHTML =
            '<div class="settings-row">' +
                '<div>' +
                    '<div class="settings-row-label">当前版本</div>' +
                    '<div class="settings-row-meta" id="settings-current-version">加载中...</div>' +
                '</div>' +
                '<span class="settings-row-value" id="settings-update-badge">-</span>' +
            '</div>' +
            '<div id="settings-update-detail" class="hidden settings-update-detail">' +
                '<div class="settings-row-meta" id="settings-update-message"></div>' +
            '</div>' +
            '<div class="settings-actions settings-actions-tight">' +
                '<button class="btn btn-secondary btn-sm" id="settings-check-update">检查更新</button>' +
                '<button class="btn btn-secondary btn-sm" id="settings-export-diagnostics">导出诊断信息</button>' +
            '</div>';

        section.appendChild(card);
        parent.appendChild(section);

 // 绑定事件
        var checkBtn = $('settings-check-update');
        if (checkBtn) checkBtn.addEventListener('click', _handleCheckUpdate);

        var exportDiagBtn = $('settings-export-diagnostics');
        if (exportDiagBtn) exportDiagBtn.addEventListener('click', _handleExportDiagnostics);

        var debugBox = $('settings-debug-enabled');
        if (debugBox) debugBox.addEventListener('change', _handleDebugToggle);
    }

 /**
 * 处理"启用调试"开关改动（v2.0 M7）。
 *
 * 语义: 立即写库（settings.debug_enabled），但 UI 生效需重启——
 * 不即时改动侧边栏/高级设置页签，避免用户误以为马上生效。
 */
    async function _handleDebugToggle() {
        var debugBox = $('settings-debug-enabled');
        if (!debugBox) return;
        var next = debugBox.checked;
        try {
            await RamariaApi.config.updateSetting('debug_enabled', next ? 'true' : 'false');
            var action = next ? '启用' : '关闭';
            RamariaToast.show('success', action + '调试已保存', '请重启 Ramaria 后生效');
        } catch (err) {
            debugBox.checked = !next; // 写库失败回滚勾选态
            RamariaToast.show('error', '保存失败', err.message || '未知错误');
        }
    }

 /**
 * 处理"检查更新"按钮点击。
 *
 * 流程:
 * 1. 调用 RamariaApi.diagnostics.checkUpdate。
 * 2. 根据返回结果显示状态：最新版本 / 新版本可用 / 检查失败。
 * 3. 有新版本时显示 Release URL（点击可打开浏览器）。
 */
    async function _handleCheckUpdate() {
        var checkBtn = $('settings-check-update');
        var badgeEl = $('settings-update-badge');
        var detailEl = $('settings-update-detail');
        var msgEl = $('settings-update-message');

        if (checkBtn) {
            checkBtn.disabled = true;
            checkBtn.textContent = '检查中...';
        }

        try {
            var result = await RamariaApi.diagnostics.checkUpdate();

 // 更新版本显示
            var versionEl = $('settings-current-version');
            if (versionEl) versionEl.textContent = 'v' + (result.currentVersion || '?');

            if (result.error) {
 // 检查失败
                if (badgeEl) {
                    badgeEl.textContent = '⚠ 检查失败';
                    badgeEl.className = 'settings-row-value text-pink';
                }
                if (detailEl) detailEl.classList.remove('hidden');
                if (msgEl) {
                    // 多行错误消息转为带换行的 HTML（v1.5 M6：先转义再拼接，防注入）
                    msgEl.innerHTML = RamariaEscape.escapeHtml(result.error).replace(/\n/g, '<br>');
                }
                // Toast 只显示首行摘要
                var firstLine = result.error.split('\n')[0];
                RamariaToast.show('warning', '检查更新失败', firstLine);
            } else if (result.updateAvailable) {
                // 新版本可用
                if (badgeEl) {
                    badgeEl.textContent = '↑ 可更新';
                    badgeEl.className = 'settings-row-value text-green';
                }
                if (detailEl) detailEl.classList.remove('hidden');
                if (msgEl) {
                    // v1.5 M6 安全修复：latestVersion/releaseUrl/releaseNotesPreview
                    // 均来自 GitHub API（远程内容），先转义再拼 HTML（href 属性同样转义）
                    var releaseHtml = '发现新版本: <strong>' + RamariaEscape.escapeHtml(result.latestVersion || '?') + '</strong>';
                    if (result.releaseUrl) {
                        releaseHtml += ' — <a href="' + RamariaEscape.escapeHtml(result.releaseUrl) +
                            '" target="_blank" rel="noopener" class="settings-about-link">前往下载</a>';
                    }
                    if (result.releaseNotesPreview) {
                        releaseHtml += '<br><small class="text-tertiary">' +
                            RamariaEscape.escapeHtml(result.releaseNotesPreview).replace(/\n/g, '<br>') + '</small>';
                    }
                    msgEl.innerHTML = releaseHtml;
                }
                RamariaToast.show('info', '发现新版本 ' + (result.latestVersion || ''));
            } else {
                // 已是最新
                if (badgeEl) {
                    badgeEl.textContent = '✓ 已是最新';
                    badgeEl.className = 'settings-row-value text-green';
                }
                if (detailEl) detailEl.classList.add('hidden');
                RamariaToast.show('success', '已是最新版本');
            }
        } catch (err) {
            console.error('[SettingsView] 检查更新失败:', err);
            if (badgeEl) {
                badgeEl.textContent = '⚠ 检查失败';
                badgeEl.className = 'settings-row-value text-pink';
            }
            if (detailEl) detailEl.classList.remove('hidden');
            if (msgEl) {
                var errText = err.message || '未知错误';
                msgEl.innerHTML = RamariaEscape.escapeHtml(errText).replace(/\n/g, '<br>');
            }
            RamariaToast.show('error', '检查更新失败', (err.message || '未知错误').split('\n')[0]);
        } finally {
            if (checkBtn) {
                checkBtn.disabled = false;
                checkBtn.textContent = '检查更新';
            }
        }
    }

 /**
 * 处理"导出诊断信息"按钮点击。
 *
 * 流程:
 * 1. 调用 RamariaApi.diagnostics.exportDiagnostics。
 * 2. 后端弹出原生保存对话框。
 * 3. 用户确认后收集并打包 zip 文件。
 */
    async function _handleExportDiagnostics() {
        var exportBtn = $('settings-export-diagnostics');
        if (exportBtn) {
            exportBtn.disabled = true;
            exportBtn.textContent = '收集中...';
        }

        try {
            var result = await RamariaApi.diagnostics.exportDiagnostics();

 // 检查是否有收集警告
            if (result.warnings && result.warnings.length > 0) {
 // 有部分数据未能收集，显示警告
                var warningText = result.warnings.join('\n');
                RamariaToast.show(
                    'warning',
                    '诊断已导出（部分信息缺失）',
                    (result.fileSizeDisplay || '') + ' — ' + (result.outputPath || '完成') + '\n\n' + warningText
                );
            } else {
                RamariaToast.show(
                    'success',
                    '诊断信息已导出',
                    (result.fileSizeDisplay || '') + ' — ' + (result.outputPath || '完成')
                );
            }
        } catch (err) {
 // 用户取消操作时静默忽略
            if (err.message && err.message.indexOf('取消') !== -1) {
                console.log('[SettingsView] 用户取消了诊断导出');
                return;
            }
            console.error('[SettingsView] 诊断导出失败:', err);
            RamariaToast.show('error', '导出失败', err.message || '未知错误');
        } finally {
            if (exportBtn) {
                exportBtn.disabled = false;
                exportBtn.textContent = '导出诊断信息';
            }
        }
    }

 // =========================================================
 // 关于区块
 // =========================================================

    function _renderAboutSection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">ℹ️ 关于</div>';

        var about = document.createElement('div');
        about.className = 'settings-about';
        about.innerHTML =
            '<div class="settings-about-logo" aria-hidden="true">🪸</div>' +
            '<div class="settings-about-name">Ramaria</div>' +
            '<div class="settings-about-version" id="settings-about-version">加载中...</div>' +
            '<div class="settings-about-desc">' +
                '个人 AI 陪伴记忆系统<br>' +
                'Rust + Tauri 2 重构版' +
            '</div>' +
            '<div class="settings-about-links">' +
                '<a class="settings-about-link" href="https://github.com/entergirl/Ramaria-s" target="_blank" rel="noopener">GitHub</a>' +
                '<span class="text-tertiary">·</span>' +
                '<a class="settings-about-link" href="#" target="_blank" rel="noopener">MIT License</a>' +
            '</div>';

        section.appendChild(about);
        parent.appendChild(section);
    }

 // =========================================================
 // 高级设置（v1.4 M6）
 // =========================================================

    /**
     * 高级配置组元数据（字段默认值与 ramaria-core/src/config.rs 的 Default 实现对齐）。
     *
     * 组约定:
     * - `key`: 组标识（DOM id 前缀；与配置段名不要求一致，如 `l1-progressive`）。
     * - `section`: 本组配置在完整 `RamariaConfig` JSON 中的段路径（数组）。
     *   字段的 `path` 为**组内相对路径**，读写时的完整路径 = `section.concat(path)`
     *   （见 [_advConfigPath]）。缺失该前缀会导致读写落到错误的键上：
     *   读取恒为空、保存静默无效（历史缺陷修复点）。
     * - `title` / `desc`: 分组标题与说明（静态文案，直接进 innerHTML）。
     *
     * 字段约定:
     * - `path`: 组内相对路径（数组，支持嵌套；完整路径由 `section` 前缀拼接）。
     * - `type`: `number` | `bool` | `whitelist` | `order`。
     * - `def`: 默认值（恢复默认与默认值标注用；`whitelist` / `order` 为数组）。
     * - `options`: `whitelist` / `order` 使用（可选值列表 {value, label}）。
     * - `order` 语义：数组顺序 = 保留优先级（高优先在前），UI 通过上/下移调整，
     *   与 `ramaria-core` 的 `[injection_budget].order`（InjectionSlot 数组）对应。
     */
    var _ADVANCED_GROUPS = [
        {
            key: 'retrieval',
            section: ['retrieval'],
            title: '🔍 检索参数',
            desc: '控制 L0/L1/L2 检索数量、相似度阈值与多通道融合权重，影响记忆召回质量。',
            fields: [
                { path: ['l0_window_size'], label: 'L0 滑动窗口', type: 'number', min: 1, def: 3, hint: 'L0 滑动窗口大小' },
                { path: ['l0_retrieve_top_k'], label: 'L0 检索条数', type: 'number', min: 0, def: 3, hint: 'L0 检索返回条数' },
                { path: ['l1_retrieve_top_k'], label: 'L1 检索条数', type: 'number', min: 0, def: 4, hint: 'L1 检索返回条数' },
                { path: ['l2_retrieve_top_k'], label: 'L2 检索条数', type: 'number', min: 0, def: 2, hint: 'L2 检索返回条数' },
                { path: ['similarity_threshold'], label: '相似度阈值', type: 'number', step: 0.05, min: 0, max: 1, def: 0.6, hint: '余弦距离超过此值视为不相关' },
                { path: ['rrf_k'], label: 'RRF 平滑系数', type: 'number', min: 1, def: 60, hint: 'RRF 融合平滑系数 k' },
                { path: ['bm25_weight'], label: 'BM25 通道权重', type: 'number', step: 0.1, min: 0, def: 1.0, hint: 'BM25 通道权重' },
                { path: ['graph_weight'], label: '图谱通道权重', type: 'number', step: 0.1, min: 0, def: 0.8, hint: '图谱通道权重' },
                { path: ['retrieval_weight_l2'], label: 'L2 排序权重', type: 'number', step: 0.1, min: 0, def: 0.8, hint: '<1.0 表示 L2 优先展示' },
                { path: ['retrieval_weight_l1'], label: 'L1 排序权重', type: 'number', step: 0.1, min: 0, def: 1.0, hint: 'L1 结果排序权重' },
                // —— v2.0 新增：向量/关键词通道与摘要路 RAG 参数（阶段一不定稿，机制开关先接入）——
                { path: ['enable_vector'], label: '向量通道开关', type: 'bool', def: true, hint: 'false = 仅 BM25 + 图谱参与 RRF 融合' },
                { path: ['enable_keyword_channel'], label: '关键词镜像通道', type: 'bool', def: true, hint: 'false = 回退三通道（BM25 + 向量 + 图谱）' },
                { path: ['keyword_weight'], label: '关键词通道权重', type: 'number', step: 0.1, min: 0, def: 1.0, hint: '关键词镜像通道 RRF 权重（1.0 与向量同权）' },
                { path: ['narrative_weighted'], label: '脉络加权注入', type: 'bool', def: true, hint: 'v1.7 B4：按时间×话题相关性融合排序；false 回退取最近 N 条' },
                { path: ['narrative_top_k'], label: '脉络注入条数', type: 'number', min: 0, def: 3, hint: '脉络加权后注入的最大条数' },
                { path: ['rag_max_memories'], label: 'RAG 上下文条数', type: 'number', min: 0, def: 5, hint: '摘要路记忆上下文最大条目数' },
                { path: ['rag_max_summary_chars'], label: 'RAG 单条摘要字符', type: 'number', min: 0, def: 120, hint: '单条记忆摘要最大字符数' },
                { path: ['rag_share_threshold_user'], label: 'Persona-Aware user 阈值', type: 'number', step: 0.05, min: 0, max: 1, def: 0.3, hint: 'user 类人格最低 share 过滤阈值' },
                { path: ['rag_share_threshold_char'], label: 'Persona-Aware 角色阈值', type: 'number', step: 0.05, min: 0, max: 1, def: 0.5, hint: 'char/anim/oc/hist 类最低 share 过滤阈值' },
                { path: ['rag_share_threshold_rama'], label: 'Persona-Aware rama 阈值', type: 'number', step: 0.05, min: 0, max: 1, def: 0.0, hint: 'rama 类最低 share 阈值（0 = 全量）' },
                { path: ['rag_include_graph_entities'], label: '上下文含图谱实体', type: 'bool', def: true, hint: '摘要路上下文格式化是否包含图谱实体' },
            ],
        },
        {
            key: 'decay',
            section: ['decay'],
            title: '⏳ 记忆衰减',
            desc: 'Ebbinghaus 遗忘曲线参数，控制记忆随时间衰减的速度。',
            fields: [
                { path: ['s_l0'], label: 'L0 稳定性系数', type: 'number', min: 1, def: 10, hint: '细节信息衰减最快' },
                { path: ['s_l1'], label: 'L1 稳定性系数', type: 'number', min: 1, def: 30, hint: 'L1 稳定性' },
                { path: ['s_l2'], label: 'L2 稳定性系数', type: 'number', min: 1, def: 60, hint: '聚合摘要衰减最慢' },
                { path: ['enable_access_boost'], label: '访问加成', type: 'bool', def: true, hint: '近期访问过的记忆保留率加成' },
                { path: ['recent_boost_days'], label: '近期访问加成天数', type: 'number', min: 0, def: 7, hint: '近期访问加成窗口' },
                { path: ['recent_boost_floor'], label: '近期保留率下限', type: 'number', step: 0.05, min: 0, max: 1, def: 0.5, hint: '近期访问保留率下限' },
                { path: ['salience_multiplier'], label: 'Salience 加成系数', type: 'number', step: 0.1, min: 0, def: 0.5, hint: 'S_adjusted = S × (1 + salience × multiplier)' },
            ],
        },
        {
            key: 'thresholds',
            section: ['thresholds'],
            title: '🎚️ 记忆层触发阈值',
            desc: '控制 L2 合并与 L3 推断的触发条件（计数 + 时间双路径）。',
            fields: [
                { path: ['l2_trigger_count'], label: 'L2 触发条数', type: 'number', min: 1, def: 5, hint: '未吸收 L1 达到此条数触发 L2 合并' },
                { path: ['l2_trigger_days'], label: 'L2 触发天数', type: 'number', min: 1, def: 7, hint: '最早未吸收 L1 超过此天数触发 L2' },
                { path: ['l3_trigger_count'], label: 'L3 触发条数', type: 'number', min: 1, def: 10, hint: '未吸收事件达到此条数触发 L3 推断' },
                { path: ['l3_trigger_days'], label: 'L3 触发天数', type: 'number', min: 1, def: 30, hint: '最早未吸收事件超过此天数触发 L3' },
                { path: ['cluster_delay_ms'], label: 'LLM 请求间隔（毫秒）', type: 'number', min: 0, def: 800, hint: '批量 LLM 请求间最小间隔（L1 导入/空闲封存摘要与 L2 事件提取共用），避免触发远程 API 速率限制（DeepSeek 建议调大）' },
            ],
        },
        {
            key: 'index',
            section: ['index'],
            title: '🗂️ 索引与 BM25',
            desc: 'BM25 增量合并与周期性重建节奏。',
            fields: [
                { path: ['bm25_incremental_threshold'], label: '增量合并阈值', type: 'number', min: 1, def: 10, hint: '缓冲区积累超过此条数触发合并' },
                { path: ['bm25_rebuild_interval'], label: '重建间隔（秒）', type: 'number', min: 10, def: 300, hint: 'BM25 定时重建检查间隔' },
            ],
        },
        {
            key: 'logging',
            section: ['logging'],
            title: '📜 日志',
            desc: '日志记录级别控制。',
            fields: [
                { path: ['log_full_prompt'], label: '记录完整 Prompt', type: 'bool', def: false, hint: '预留项：当前版本未接线——无论开关取值都不会记录完整 Prompt（后续启用前会先更新隐私说明）' },
            ],
        },
        {
            key: 'inference',
            section: ['inference'],
            title: '🔮 L3 推断',
            desc: 'Phase B/C 推断参数（温度、证据阈值、置信度、漂移检测、全量校准）。',
            fields: [
                { path: ['inferrer', 'temperature'], label: '推断温度', type: 'number', step: 0.1, min: 0, max: 2, def: 0.3, hint: 'Phase B LLM 生成温度' },
                { path: ['inferrer', 'max_tokens'], label: '最大输出 tokens', type: 'number', min: 128, def: 2048, hint: 'Phase B LLM 最大输出' },
                { path: ['inferrer', 'low_evidence_threshold'], label: '低证据阈值', type: 'number', step: 0.5, min: 0, def: 5.0, hint: '小样本分类证据阈值' },
                { path: ['inferrer', 'step_max_tokens'], label: '每步最大 tokens', type: 'number', min: 128, def: 2048, hint: 'Phase B 每步最大输出' },
                { path: ['confidence', 'stability_s'], label: '置信度稳定性 S', type: 'number', step: 1, min: 1, def: 60, hint: 'L2 层稳定性系数（Ebbinghaus）' },
                { path: ['confidence', 'min_decay'], label: '时间衰减保底', type: 'number', step: 0.01, min: 0, max: 1, def: 0.01, hint: '置信度时间衰减保底值' },
                { path: ['drift', 'alpha'], label: '漂移显著性水平', type: 'number', step: 0.01, min: 0.001, max: 1, def: 0.05, hint: 'Wasserstein 漂移检测显著性（锁定值）' },
                { path: ['drift', 'n_permutations'], label: '置换检验次数', type: 'number', min: 100, def: 1000, hint: '置换检验次数（锁定值）' },
                { path: ['calibration', 'round_threshold'], label: '校准轮次阈值', type: 'number', min: 1, def: 10, hint: '增量更新轮次阈值' },
                { path: ['calibration', 'event_doubling_ratio'], label: '事件翻倍比例', type: 'number', step: 0.1, min: 1, def: 2.0, hint: '事件量翻倍比例阈值' },
                { path: ['calibration', 'diff_alert_ratio'], label: '差异告警比例', type: 'number', step: 0.05, min: 0, max: 1, def: 0.3, hint: '差异告警比例' },
            ],
        },
        {
            key: 'event_extraction',
            section: ['event_extraction'],
            title: '📇 事件提取',
            desc: 'L1→L2 事件提取器的 LLM 参数（独立于对话参数，JSON 输出需更大 token 预算）。',
            fields: [
                { path: ['temperature'], label: '提取温度', type: 'number', step: 0.1, min: 0, max: 2, def: 0.3, hint: '事件提取 LLM 温度' },
                { path: ['max_tokens'], label: '最大输出 tokens', type: 'number', min: 256, def: 8192, hint: '事件 JSON 输出预算' },
                { path: ['max_events'], label: '单簇最大事件数', type: 'number', min: 1, def: 5, hint: '单簇最多提取的事件数' },
                { path: ['degraded_confidence_enabled'], label: '降级事件动态置信度', type: 'bool', def: true, hint: 'true = 降级事件按 min(0.59, 0.35 + 0.02 × n_l1) 计算置信度（恒 tentative）；false = 回退固定 0.5' },
            ],
        },
        {
            key: 'utt',
            section: ['utt'],
            title: '💬 utt 原文通道',
            desc: '原文话语块切分、检索与注入参数。原文是最高敏感层，白名单外 persona 不注入。',
            fields: [
                { path: ['enabled'], label: '启用原文通道', type: 'bool', def: true, hint: '关闭后行为回退 v1.3（不注入原文片段）' },
                { path: ['theta_gap_minutes'], label: '切分时间间隙（分钟）', type: 'number', min: 1, def: 10, hint: '相邻消息间隔超过此值切分为新块（窄切分，更细粒度）' },
                { path: ['max_msgs_per_block'], label: '单块最大消息数', type: 'number', min: 1, def: 80, hint: '超过此条数强制切分（更大块、更少切分）' },
                { path: ['retrieve_top_k'], label: '检索块数 top_k', type: 'number', min: 0, def: 3, hint: '对话时检索返回的 utt 块数量' },
                { path: ['max_block_chars'], label: '注入字符预算', type: 'number', min: 50, def: 1500, hint: '原文片段注入预算（超限按相似度丢整块）' },
                {
                    path: ['persona_kind_whitelist'],
                    label: '原文白名单（persona 类型）',
                    type: 'whitelist',
                    def: ['char', 'anim', 'oc', 'hist'],
                    options: [
                        { value: 'char', label: '角色' },
                        { value: 'anim', label: '动画' },
                        { value: 'oc', label: '原创 OC' },
                        { value: 'hist', label: '历史人物' },
                    ],
                    hint: '白名单外的 persona（助手/系统类）不注入原文',
                },
            ],
        },
        {
            key: 'examples',
            section: ['examples'],
            title: '🎭 示例注入',
            desc: 'Few-shot 示例的自学习抽取、评分轮换与兜底注入参数。',
            fields: [
                { path: ['enabled'], label: '启用示例注入', type: 'bool', def: true, hint: '关闭后回退 v1.3 静态 selected 查询' },
                { path: ['max_examples'], label: '最大示例条数', type: 'number', min: 1, def: 5, hint: '注入时的最大示例条数' },
            ],
        },
        {
            key: 'bridge',
            section: ['bridge'],
            title: '🌉 会话桥接',
            desc: '新会话加载上一会话尾部原文，保持对话连贯性。',
            fields: [
                { path: ['enabled'], label: '启用桥接', type: 'bool', def: true, hint: '关闭后新会话不加载桥接（等同 v1.3）' },
                { path: ['max_chars'], label: '桥接字符预算', type: 'number', min: 50, def: 800, hint: '超限从头部截断、保最近' },
            ],
        },
        {
            key: 'knowledge',
            section: ['knowledge'],
            title: '🧩 知识层（fact 路）',
            desc: '事件→知识事实抽取、判重与注入参数；本组为知识路独立检索参数（与摘要路/原文路互不串扰）。',
            fields: [
                { path: ['auto_fact_detect'], label: '自动事实检测总开关', type: 'bool', def: false, hint: 'v2.0：开启后抽取走常规轨道 + 增强轨道；false 与 v1.7 完全一致' },
                { path: ['detector_enabled'], label: '规则判定器开关', type: 'bool', def: true, hint: 'false = 不检索注入（零新增 LLM 调用）' },
                { path: ['retrieve_top_k'], label: '检索条数上限', type: 'number', min: 0, def: 0, hint: '0 = 与 v1.7 等价（不按条数截断，仅按预算注入）' },
                { path: ['retrieve_threshold'], label: '检索路由阈值', type: 'number', step: 0.05, min: 0, max: 1, def: 0.0, hint: '0.0 = 与 v1.7 等价（沿用既有判定口径）' },
                { path: ['dedup_cosine_threshold'], label: '判重余弦阈值', type: 'number', step: 0.05, min: 0, max: 1, def: 0.85, hint: '同 field 语义 ≥0.85 且关键词交集 ≥1 判重复' },
                { path: ['dedup_keyword_min'], label: '判重关键词下限', type: 'number', min: 1, def: 1, hint: '≥1 个共同词参与判重' },
                { path: ['corroboration_cosine_threshold'], label: '互证余弦阈值', type: 'number', step: 0.05, min: 0, max: 1, def: 0.7, hint: '≥2 独立事件语义 ≥0.7 且 valence 一致才互证' },
                { path: ['injection_budget_chars'], label: '事实卡片注入预算（字符）', type: 'number', min: 0, def: 800, hint: '超预算保前部' },
                { path: ['volatile_halflife_days'], label: '动态事实时效半衰期（天）', type: 'number', min: 1, def: 30, hint: '随事件时间衰减' },
            ],
        },
        {
            key: 'style',
            section: ['style'],
            title: '🎨 说话风格（表达层）',
            desc: '五维风格统计、显著性检验与自动规则生成（A3）；关闭整链路回退 v1.6 语义。',
            fields: [
                { path: ['enabled'], label: '风格统计总开关', type: 'bool', def: true, hint: 'false = 不统计、不生成规则、不注入' },
                { path: ['auto_translate'], label: 'LLM 离线翻译增强', type: 'bool', def: true, hint: 'false = 仅模板拼接（确定性、零 LLM 依赖）' },
                { path: ['sample_fallback'], label: '小样本原文样例兜底', type: 'bool', def: true, hint: '样本不足时写入 SpeakingStyle 样例事实供展示' },
                { path: ['keyword_dict'], label: '关键词体系衔接', type: 'bool', def: true, hint: '词表存在时风格候选走词典增强；否则回退纯 bigram' },
                { path: ['min_sample_count'], label: '样本量阈值 n_p', type: 'number', min: 1, def: 200, hint: '低于此值标注数据不足，不生成规则、不注入' },
                { path: ['top_n'], label: '口癖/话题词 Top-N', type: 'number', min: 1, def: 10, hint: '文档范围 10~20' },
                { path: ['relative_boost_ratio'], label: '相对超频比阈值', type: 'number', step: 0.1, min: 0, def: 2.0, hint: 'persona 频率 / 全局频率 > 此值视为口癖' },
                { path: ['min_frequency'], label: '显著项最小频次', type: 'number', min: 1, def: 5, hint: '频次低于此值不参与显著性判定' },
                { path: ['z_critical'], label: 'z 临界值', type: 'number', step: 0.1, min: 0, def: 2.0, hint: '|z| ≥ 此值判定统计显著' },
            ],
        },
        {
            key: 'injection_budget',
            section: ['injection_budget'],
            title: '📦 注入协调预算（v2.0）',
            desc: 'RAG 摘要与四层注入纳入同一 token 池：超限按 order 从低优先整块丢弃。默认关闭时行为与既有版本等价。',
            fields: [
                { path: ['enabled'], label: '协调预算开关', type: 'bool', def: false, hint: 'false = 走既有独立预算路径（回归红线）' },
                { path: ['max_injection_tokens'], label: '注入总 token 上限', type: 'number', min: 0, def: 1000, hint: '固定骨架（能力边界/角色/时间）不计入池' },
                { path: ['max_rag_tokens'], label: 'RAG 独立 token 上限', type: 'number', min: 0, def: 0, hint: '0 = 不设独立上限，仅受总池约束' },
                {
                    path: ['order'],
                    label: '通道保留优先级（高优先在前）',
                    type: 'order',
                    def: ['rag', 'behavior', 'knowledge', 'style', 'memory'],
                    options: [
                        { value: 'rag', label: 'RAG 摘要' },
                        { value: 'behavior', label: '行为层' },
                        { value: 'knowledge', label: '知识层' },
                        { value: 'style', label: '表达层' },
                        { value: 'memory', label: '脉络层' },
                    ],
                    hint: '超预算时从列表末尾（最低优先）开始整块丢弃；未列出的通道视为最低优先',
                },
            ],
        },
        {
            key: 'layer_dedup',
            section: ['layer_dedup'],
            title: '🧾 层间去重仲裁（v2.0）',
            desc: '同一事实跨层去重与冲突仲裁（引用级 + 内容级覆盖）。默认关闭时走既有引用级去重路径。',
            fields: [
                { path: ['enabled'], label: '层间去重仲裁开关', type: 'bool', def: false, hint: 'false = 走既有引用级去重（行为不变）' },
            ],
        },
        {
            key: 'l1-progressive',
            section: ['l1', 'progressive'],
            title: '📚 渐进式摘要 B3（v2.0）',
            desc: '长会话（消息数/时间跨度超阈值）在封存时按段生成多个 L1。默认开启，关闭时回退 v1.6 行为。',
            fields: [
                { path: ['enabled'], label: '渐进式摘要开关', type: 'bool', def: true, hint: 'false = 整会话/按 utt 切分（v1.6）' },
                { path: ['msg_threshold'], label: '消息数触发阈值', type: 'number', min: 2, def: 100, hint: '超过此条数触发分段' },
                { path: ['span_hours'], label: '时间跨度阈值（小时）', type: 'number', min: 1, def: 24, hint: '首末消息跨度超过此值触发分段' },
                { path: ['tail_msg_count'], label: '尾段覆盖消息数', type: 'number', min: 1, def: 60, hint: '按此条数切段、全段生成（尾段覆盖最新）' },
            ],
        },
        {
            key: 'inference-upgrade',
            section: ['inference', 'upgrade'],
            title: '🔃 画像升级开关（v2.0）',
            desc: 'Phase A 后分层收缩与 Phase B/C 画像升级各增量的独立开关；全部关闭时画像输出回退旧版行为。',
            fields: [
                { path: ['cross_version_threshold_085'], label: '跨版本簇阈值 0.85', type: 'bool', def: true, hint: 'false = 回退旧值 0.75' },
                { path: ['cold_start_cross_user_prior'], label: '跨用户冷启动先验', type: 'bool', def: true, hint: 'false = 回退 persona 内先验' },
                { path: ['drift_restore_real_distribution'], label: '漂移检测真实恢复', type: 'bool', def: true, hint: 'false = 漂移检测整体显式跳过' },
                { path: ['causal_latency_emotion_trend'], label: '因果时延+情绪走势', type: 'bool', def: true, hint: 'false = 回退 v1.7 仅链长/循环模式' },
            ],
        },
    ];

    /**
     * 渲染高级设置区块：风险提示条 + 全部配置组表单。
     */
    function _renderAdvancedSection(parent) {
        var risk = document.createElement('div');
        risk.className = 'settings-risk-banner';
        risk.textContent =
            '⚠️ 高级设置面向进阶用户与排障场景。修改以下参数可能影响检索与推断质量，' +
            '非排障场景请保持默认值；可单项或一键全部恢复默认。';
        parent.appendChild(risk);

        for (var i = 0; i < _ADVANCED_GROUPS.length; i++) {
            _renderAdvancedGroup(parent, _ADVANCED_GROUPS[i]);
        }

        // ── 全局恢复默认栏（v1.5 M6 U 项功能增强）：单开一栏，点击直接执行（无弹窗）──
        var resetSection = document.createElement('div');
        resetSection.className = 'settings-section';
        resetSection.innerHTML =
            '<div class="settings-section-title">⚙️ 恢复默认</div>' +
            '<div class="settings-section-desc">将下方全部高级参数恢复为出厂默认值并立即保存（仅影响高级设置页参数，基础设置与后端连接配置不受影响）。</div>' +
            '<div class="settings-card">' +
                '<div class="settings-adv-reset-row">' +
                    '<span class="settings-adv-reset-desc">确认后立即生效，无需逐项操作。</span>' +
                    '<button class="btn btn-secondary btn-sm" id="settings-adv-reset-all">全部恢复默认</button>' +
                '</div>' +
            '</div>';
        parent.appendChild(resetSection);

        var resetAllBtn = $('settings-adv-reset-all');
        if (resetAllBtn) {
            resetAllBtn.addEventListener('click', _handleAdvancedResetAll);
        }
    }

    /**
     * 按元数据渲染一个高级配置组。
     */
    function _renderAdvancedGroup(parent, group) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">' + group.title + '</div>' +
            '<div class="settings-section-desc">' + group.desc + '</div>';

        var card = document.createElement('div');
        card.className = 'settings-card';
        var html = '';
        for (var i = 0; i < group.fields.length; i++) {
            var f = group.fields[i];
            var fid = _advFieldId(group, f);
            if (f.type === 'bool') {
                // 默认开的选项默认选中（v1.5 M6 U 项增强）：渲染时按 def 预置 checked，
                // 配置回显（_fillAdvancedForm）再覆盖为实际值，避免加载前/失败时状态与默认不符
                html += '<div class="settings-form-group">' +
                    '<label class="settings-form-label">' +
                        '<input type="checkbox" id="' + fid + '"' + (f.def ? ' checked' : '') + ' /> ' + f.label +
                    '</label>' +
                    '<div class="settings-form-hint">' + f.hint + '（默认：' + (f.def ? '开' : '关') + '）</div>' +
                '</div>';
            } else if (f.type === 'whitelist') {
                html += '<div class="settings-form-group">' +
                    '<label class="settings-form-label">' + f.label + '</label>' +
                    '<div class="settings-form-whitelist">';
                for (var w = 0; w < f.options.length; w++) {
                    var opt = f.options[w];
                    var defOn = f.def.indexOf(opt.value) !== -1;
                    html += '<label class="settings-form-inline-label">' +
                        '<input type="checkbox" id="' + fid + '-' + opt.value + '" data-value="' + opt.value + '"' +
                            (defOn ? ' checked' : '') + ' /> ' +
                        opt.label +
                    '</label>';
                }
                html += '</div>' +
                    '<div class="settings-form-hint">' + f.hint + '（默认：' + f.def.join(', ') + '）</div>' +
                '</div>';
            } else if (f.type === 'order') {
                // 优先级列表：上下移动调整顺序（超预算时从末尾开始整块丢弃）
                html += '<div class="settings-form-group">' +
                    '<label class="settings-form-label">' + f.label + '</label>' +
                    '<div class="settings-adv-order" id="' + fid + '" role="list">' +
                        _orderItemsHtml(f, f.def) +
                    '</div>' +
                    '<div class="settings-form-hint">' + f.hint + '（默认：' + f.def.join(' → ') + '）</div>' +
                '</div>';
            } else {
                // 数值输入框：初始 value 预置默认值并以浅色字符显示（is-default），
                // 提示"当前为默认值"；用户输入不同值后自动转深色（见 input 事件同步）
                html += '<div class="settings-form-group">' +
                    '<label class="settings-form-label">' + f.label + '</label>' +
                    '<input class="settings-form-input is-default" id="' + fid + '" type="number" value="' + f.def + '"' +
                        (f.step ? ' step="' + f.step + '"' : '') +
                        (f.min !== undefined ? ' min="' + f.min + '"' : '') +
                        (f.max !== undefined ? ' max="' + f.max + '"' : '') +
                    ' />' +
                    '<div class="settings-form-hint">' + f.hint + '（默认：' + f.def + '）</div>' +
                '</div>';
            }
        }
        html += '<div class="settings-save-hint">' +
            '<button class="btn btn-primary btn-sm" id="settings-adv-save-' + group.key + '">保存</button> ' +
            '<button class="btn btn-secondary btn-sm" id="settings-adv-reset-' + group.key + '">恢复默认</button>' +
        '</div>';
        card.innerHTML = html;
        section.appendChild(card);
        parent.appendChild(section);

        var saveBtn = $('settings-adv-save-' + group.key);
        if (saveBtn) {
            saveBtn.addEventListener('click', function () { _handleAdvancedSave(group); });
        }
        var resetBtn = $('settings-adv-reset-' + group.key);
        if (resetBtn) {
            resetBtn.addEventListener('click', function () { _handleAdvancedReset(group); });
        }

        // 数值输入框：输入时同步"默认值浅色字符"状态（值=默认 → 浅色，否则深色）
        for (var j = 0; j < group.fields.length; j++) {
            var field = group.fields[j];
            if (field.type === 'number') {
                var numInput = $(_advFieldId(group, field));
                if (numInput) {
                    numInput.addEventListener('input', (function (input, fld) {
                        return function () { _syncAdvancedInputDefault(input, fld); };
                    })(numInput, field));
                }
            }
        }

        // 优先级列表：事件委托（容器内上下移动，重建行后无需重新绑定）
        for (var k = 0; k < group.fields.length; k++) {
            if (group.fields[k].type === 'order') {
                var orderBox = $(_advFieldId(group, group.fields[k]));
                if (orderBox) _bindOrderField(orderBox);
            }
        }

        // log_full_prompt 开启需显式隐私确认
        var logBox = $('settings-adv-logging-log_full_prompt');
        if (logBox) {
            logBox.addEventListener('change', function () {
                if (!logBox.checked) return;
                RamariaModal.show({
                    title: '⚠️ 隐私确认',
                    body: '「记录完整 Prompt」为预留项：当前版本未接线，开启或关闭都不会把完整 prompt ' +
                        '写入日志。确认仅保存开关取值，供后续版本启用时生效（届时会先更新隐私说明）。',
                    actions: [
                        { label: '取消', action: 'cancel', className: 'btn btn-secondary' },
                        { label: '确认开启', action: 'confirm', className: 'btn btn-danger' },
                    ],
                    onAction: function (action) {
                        if (action !== 'confirm') {
                            logBox.checked = false;
                        }
                    },
                });
            });
        }
    }

    /**
     * 高级字段的 DOM id。
     */
    function _advFieldId(group, f) {
        return 'settings-adv-' + group.key + '-' + f.path.join('-');
    }

    /**
     * 计算字段在完整配置对象中的绝对路径。
     *
     * 说明:
     * - 元数据里字段 `path` 为组内相对路径，组级 `section` 给出配置段前缀；
     *   读写完整 `RamariaConfig` JSON 必须使用本函数拼接结果。
     * - 历史缺陷：早期实现直接用 `f.path` 读写，导致 `[utt]`/`[retrieval]` 等
     *   组读到 undefined（表单恒显示默认值）、保存写到不存在的根键（静默无效）。
     *
     * 参数:
     * - `group`: 组元数据（含 section）。
     * - `f`: 字段元数据（含相对 path）。
     *
     * 返回:
     * - 绝对路径数组。
     */
    function _advConfigPath(group, f) {
        var section = Array.isArray(group.section) ? group.section : [];
        return section.concat(f.path);
    }

    /**
     * 按 value 查 `order` 字段的选项元数据（{value, label}）。
     *
     * 返回:
     * - 命中项；未命中返回 null（调用方据此过滤未知通道）。
     */
    function _orderOption(f, value) {
        for (var i = 0; i < f.options.length; i++) {
            if (f.options[i].value === value) return f.options[i];
        }
        return null;
    }

    /**
     * 归一化 `order` 字段的取值列表。
     *
     * 规则（与 ramaria-core `[injection_budget].order` 语义对齐）:
     * - 非数组/空数组 → 回退默认顺序；
     * - 过滤未知通道；重复项取首次出现位置；
     * - 配置缺失的通道按选项表顺序补到末尾，保证 UI 五项齐全、可上下移动。
     *
     * 参数:
     * - `f`: 字段元数据（含 options/def）。
     * - `value`: 配置中的当前值（可能 undefined）。
     *
     * 返回:
     * - 归一化后的通道顺序数组。
     */
    function _normalizeOrder(f, value) {
        var raw = Array.isArray(value) ? value : f.def;
        var out = [];
        for (var i = 0; i < raw.length; i++) {
            var v = raw[i];
            if (typeof v !== 'string') continue;
            if (!_orderOption(f, v)) continue;
            if (out.indexOf(v) !== -1) continue;
            out.push(v);
        }
        for (var j = 0; j < f.options.length; j++) {
            if (out.indexOf(f.options[j].value) === -1) out.push(f.options[j].value);
        }
        return out;
    }

    /**
     * 生成优先级列表的行 HTML（标签均为静态元数据，非用户输入）。
     *
     * 结构:
     * - `.settings-adv-order-item[data-value]`：一行（序号 + 名称 + 上/下移按钮）；
     * - 首行禁用「上移」、末行禁用「下移」，防止无效操作。
     */
    function _orderItemsHtml(f, values) {
        var order = _normalizeOrder(f, values);
        var html = '';
        for (var i = 0; i < order.length; i++) {
            var opt = _orderOption(f, order[i]);
            if (!opt) continue;
            var upDisabled = i === 0 ? ' disabled' : '';
            var downDisabled = i === order.length - 1 ? ' disabled' : '';
            html += '<div class="settings-adv-order-item" data-value="' + opt.value + '" role="listitem">' +
                '<span class="settings-adv-order-index">' + (i + 1) + '</span>' +
                '<span class="settings-adv-order-name">' + opt.label + '</span>' +
                '<span class="settings-adv-order-actions">' +
                    '<button type="button" class="btn btn-secondary btn-sm settings-adv-order-btn" data-dir="up" title="上移（提高保留优先级）"' + upDisabled + '>↑</button>' +
                    '<button type="button" class="btn btn-secondary btn-sm settings-adv-order-btn" data-dir="down" title="下移（降低保留优先级）"' + downDisabled + '>↓</button>' +
                '</span>' +
            '</div>';
        }
        return html;
    }

    /**
     * 绑定优先级列表的上/下移交互（事件委托，容器内行重建后无需重新绑定）。
     */
    function _bindOrderField(container) {
        container.addEventListener('click', function (ev) {
            // 沿父链定位带 data-dir 的按钮（不依赖 closest，兼容性更稳）
            var target = ev.target;
            var btn = null;
            while (target && target !== container) {
                if (target.getAttribute && target.getAttribute('data-dir')) {
                    btn = target;
                    break;
                }
                target = target.parentNode;
            }
            if (!btn) return;
            ev.preventDefault();

            var row = btn.parentNode ? btn.parentNode.parentNode : null;  // .settings-adv-order-item
            if (!row || row.parentNode !== container) return;

            var dir = btn.getAttribute('data-dir');
            if (dir === 'up' && row.previousElementSibling) {
                container.insertBefore(row, row.previousElementSibling);
            } else if (dir === 'down' && row.nextElementSibling) {
                container.insertBefore(row.nextElementSibling, row);
            }
            _refreshOrderIndexes(container);
        });
    }

    /**
     * 刷新优先级列表的序号与首尾按钮禁用态（移动后调用）。
     */
    function _refreshOrderIndexes(container) {
        var rows = container.querySelectorAll('.settings-adv-order-item');
        if (rows.length === 0) return;
        for (var i = 0; i < rows.length; i++) {
            var idxEl = rows[i].querySelector('.settings-adv-order-index');
            if (idxEl) idxEl.textContent = String(i + 1);

            var up = rows[i].querySelector('button[data-dir="up"]');
            var down = rows[i].querySelector('button[data-dir="down"]');
            if (up) up.disabled = (i === 0);
            if (down) down.disabled = (i === rows.length - 1);
        }
    }

    /**
     * 从配置对象读取嵌套路径值。
     */
    function _advGetValue(cfg, path) {
        var v = cfg;
        for (var i = 0; i < path.length; i++) {
            v = v[path[i]];
            if (v === undefined) return undefined;
        }
        return v;
    }

    /**
     * 向配置对象写入嵌套路径值。
     *
     * v1.5 M6 增强：路径中间对象缺失时自动补建（防御——配置快照可能不含
     * 某些嵌套分组，直接写入会抛 TypeError 中断「恢复全部默认」流程）。
     */
    function _advSetValue(cfg, path, value) {
        var v = cfg;
        for (var i = 0; i < path.length - 1; i++) {
            if (v[path[i]] == null || typeof v[path[i]] !== 'object') {
                v[path[i]] = {};
            }
            v = v[path[i]];
        }
        v[path[path.length - 1]] = value;
    }

    /**
     * 回显一个高级配置组（进入设置页时调用）。
     *
     * 说明（v1.5 M6 U 项增强）:
     * - 渲染时已按默认值预置控件状态；此处仅在实际配置存在对应字段时覆盖，
     *   配置缺失（undefined）时保留默认态（checkbox 保持默认选中、输入框保持默认值）。
     * - 数值输入框回显后同步"默认值浅色字符"状态。
     */
    function _fillAdvancedForm(group, cfg) {
        for (var i = 0; i < group.fields.length; i++) {
            var f = group.fields[i];
            var fid = _advFieldId(group, f);
            var value = _advGetValue(cfg, _advConfigPath(group, f));
            if (f.type === 'bool') {
                if (value !== undefined) {
                    var box = $(fid);
                    if (box) box.checked = !!value;
                }
            } else if (f.type === 'whitelist') {
                if (value !== undefined) {
                    var list = Array.isArray(value) ? value : [];
                    for (var w = 0; w < f.options.length; w++) {
                        var cb = $(fid + '-' + f.options[w].value);
                        if (cb) cb.checked = list.indexOf(f.options[w].value) !== -1;
                    }
                }
            } else if (f.type === 'order') {
                // 重建整列：归一化（过滤未知通道 + 补齐缺失项）后按配置顺序渲染
                var orderBox = $(fid);
                if (orderBox) {
                    orderBox.innerHTML = _orderItemsHtml(f, value);
                }
            } else {
                var input = $(fid);
                if (!input) continue;
                if (value !== undefined) input.value = value;
                _syncAdvancedInputDefault(input, f);
            }
        }
    }

    /**
     * 同步数值输入框的"默认值浅色字符"状态（v1.5 M6 U 项增强）。
     *
     * 规则: 输入框当前值 === 字段默认值 → 添加 `is-default`（浅色字符，
     * 提示"当前为默认值"）；否则移除（深色主文字，表示已自定义）。
     * 数值统一按字符串比较（HTML input.value 恒为字符串，避免 1 与 1.0 误判差异）。
     */
    function _syncAdvancedInputDefault(input, f) {
        if (!input) return;
        var isDefault = String(input.value) === String(f.def);
        input.classList.toggle('is-default', isDefault);
    }

    /**
     * 回显全部高级配置组。
     */
    function _fillAdvancedForms(cfg) {
        for (var i = 0; i < _ADVANCED_GROUPS.length; i++) {
            _fillAdvancedForm(_ADVANCED_GROUPS[i], cfg);
        }
    }

    /**
     * 收集一个高级配置组的当前表单值（校验后返回 {path, value} 列表；
     * 校验失败返回 null 并 toast 提示）。
     */
    function _collectAdvancedGroup(group) {
        var entries = [];
        for (var i = 0; i < group.fields.length; i++) {
            var f = group.fields[i];
            var fid = _advFieldId(group, f);
            var value;
            if (f.type === 'bool') {
                var box = $(fid);
                if (!box) continue;
                value = box.checked;
            } else if (f.type === 'whitelist') {
                var list = [];
                for (var w = 0; w < f.options.length; w++) {
                    var cb = $(fid + '-' + f.options[w].value);
                    if (cb && cb.checked) list.push(f.options[w].value);
                }
                value = list;
            } else if (f.type === 'order') {
                var orderContainer = $(fid);
                if (!orderContainer) continue;
                var orderRows = orderContainer.querySelectorAll('.settings-adv-order-item');
                var orderList = [];
                for (var k = 0; k < orderRows.length; k++) {
                    var slot = orderRows[k].getAttribute('data-value');
                    // 去重保持首次出现位置（与后端「重复项取首个位置」口径一致）
                    if (slot && orderList.indexOf(slot) === -1) orderList.push(slot);
                }
                if (orderList.length === 0) {
                    RamariaToast.show('warning', f.label + ' 不能为空');
                    return null;
                }
                value = orderList;
            } else {
                var input = $(fid);
                if (!input) continue;
                var raw = input.value;
                if (raw === '') {
                    RamariaToast.show('warning', f.label + ' 不能为空');
                    return null;
                }
                value = (f.step && f.step < 1) ? parseFloat(raw) : parseInt(raw, 10);
                if (isNaN(value)) {
                    RamariaToast.show('warning', f.label + ' 不是有效数字');
                    return null;
                }
                // 范围校验（HTML min/max 在无 form 提交时不生效，需显式检查）
                if (f.min !== undefined && value < f.min) {
                    RamariaToast.show('warning', f.label + ' 不能小于 ' + f.min);
                    return null;
                }
                if (f.max !== undefined && value > f.max) {
                    RamariaToast.show('warning', f.label + ' 不能大于 ' + f.max);
                    return null;
                }
            }
            entries.push({ path: _advConfigPath(group, f), value: value });
        }
        return entries;
    }

    /**
     * 统一处理 updateFull 双写结果（单侧写失败降级不阻塞，但需明确提示；
     * 决策记录见 docs/dev-1.5/v1.5-decisions.md）。
     *
     * 返回:
     * - `true`: 至少一侧生效（含单侧失败降级提示）。
     * - `false`: 双写均失败（致命，调用方应报错）。
     */
    function _handleUpdateFullResult(result, okMsg) {
        if (!result) return true;
        if (result.fileOk === false && result.dbOk === false) return false;
        if (result.fileOk === false) {
            RamariaToast.show('warning', okMsg + '（config.toml 同步失败，数据库侧已生效；下次启动校验会提示不一致）');
            return true;
        }
        if (result.dbOk === false) {
            RamariaToast.show('warning', okMsg + '（数据库侧同步失败，config.toml 已生效）');
            return true;
        }
        if (result.failures && result.failures.length > 0) {
            RamariaToast.show('warning', okMsg + '（部分同步项失败：' + result.failures.join('；') + '）');
            return true;
        }
        RamariaToast.show('success', okMsg);
        return true;
    }

    /**
     * 保存一个高级配置组（统一写入口双写）。
     */
    async function _handleAdvancedSave(group) {
        try {
            if (!_fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }
            var entries = _collectAdvancedGroup(group);
            if (!entries) return;

            var cfg = JSON.parse(JSON.stringify(_fullConfig));
            for (var i = 0; i < entries.length; i++) {
                _advSetValue(cfg, entries[i].path, entries[i].value);
            }

            var result = await RamariaApi.config.updateFull(cfg);
            if (!_handleUpdateFullResult(result, group.title + ' 已保存')) {
                throw new Error('配置双写均失败');
            }
            _fullConfig = cfg;
        } catch (err) {
            RamariaToast.show('error', '保存失败', err.message || '未知错误');
        }
    }

    /**
     * 恢复一个高级配置组的默认值（立即保存，统一写入口双写）。
     */
    async function _handleAdvancedReset(group) {
        try {
            if (!_fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }
            var cfg = JSON.parse(JSON.stringify(_fullConfig));
            for (var i = 0; i < group.fields.length; i++) {
                var f = group.fields[i];
                _advSetValue(cfg, _advConfigPath(group, f), JSON.parse(JSON.stringify(f.def)));
            }

            var result = await RamariaApi.config.updateFull(cfg);
            if (!_handleUpdateFullResult(result, group.title + ' 已恢复默认并保存')) {
                throw new Error('配置双写均失败');
            }
            _fullConfig = cfg;
            _fillAdvancedForm(group, cfg);
        } catch (err) {
            RamariaToast.show('error', '恢复默认失败', err.message || '未知错误');
        }
    }

    /**
     * 一键恢复全部高级设置参数为默认值（v1.5 M6 U 项功能增强）。
     *
     * 交互（2026-08-14 负责人调整：单开一栏、无确认弹窗）:
     * - 独立栏内直接点击执行（栏内已带说明文字，无需再弹窗确认）；
     * - 遍历 `_ADVANCED_GROUPS` 全部字段写回默认值并立即保存（统一写入口双写）；
     * - 保存成功后重新回显全部高级表单（触发"默认值浅色字符"状态同步）。
     */
    async function _handleAdvancedResetAll() {
        try {
            if (!_fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }
            var cfg = JSON.parse(JSON.stringify(_fullConfig));
            for (var i = 0; i < _ADVANCED_GROUPS.length; i++) {
                var group = _ADVANCED_GROUPS[i];
                for (var j = 0; j < group.fields.length; j++) {
                    var f = group.fields[j];
                    _advSetValue(cfg, _advConfigPath(group, f), JSON.parse(JSON.stringify(f.def)));
                }
            }
            var result = await RamariaApi.config.updateFull(cfg);
            if (!_handleUpdateFullResult(result, '高级设置已全部恢复默认并保存')) {
                throw new Error('配置双写均失败');
            }
            _fullConfig = cfg;
            _fillAdvancedForms(cfg);
        } catch (err) {
            RamariaToast.show('error', '恢复默认失败', err.message || '未知错误');
        }
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
            _loadVersion();

 // 加载配置
            try {
                _backendConfig = await RamariaApi.config.getBackend();
                _fillBackendForm(_backendConfig);
                RamariaStore.set('backendConfig', _backendConfig);
            } catch (err) {
                console.error('[SettingsView] 加载后端配置失败:', err);
            }

 // 加载嵌入模型配置
            try {
                var embeddingConfig = await RamariaApi.setup.getEmbeddingModel();
                _fillEmbeddingForm(embeddingConfig);
            } catch (err) {
                console.error('[SettingsView] 加载嵌入模型配置失败:', err);
            }

 // 加载完整配置并回显（v1.4 M5 会话区块 + M6 基础/高级表单 + MCP 接入）
             try {
                 _fullConfig = await RamariaApi.config.getFull();
                 _fillSessionForm(_fullConfig);
                 _fillMemoryInjectionForm(_fullConfig);
                 _fillDataDirForm(_fullConfig);
                 _fillMcpForm(_fullConfig);
                 _fillAdvancedForms(_fullConfig);
             } catch (err) {
                 console.error('[SettingsView] 加载完整配置失败:', err);
             }

 // 加载 MCP 接入信息（运行状态 + 客户端配置片段）
             await _refreshMcpInfo();

 // 加载隐私状态
             await _refreshPrivacyStatus();
        });
        _unregisterFns.push(unreg);

        unreg = RamariaRouter.registerHook('settings', 'leave', function () {
            console.log('[SettingsView] 离开视图');
            for (var i = 0; i < _unsubs.length; i++) {
                try { _unsubs[i](); } catch (_) { /* ignore */ }
            }
            _unsubs = [];
        });
        _unregisterFns.push(unreg);
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
         * - `_ADVANCED_GROUPS` 的深拷贝；调用方修改不会影响设置页内部状态。
         */
        getAdvancedGroups: function () {
            return JSON.parse(JSON.stringify(_ADVANCED_GROUPS));
        },
        /**
         * MCP 接入面板字段元数据快照（深拷贝，只读）。
         *
         * 用途:
         * - 默认值一致性回归（与 `config/default.toml` 的 `[mcp]` 组逐键比对）；
         * - 排障时查看面板认识的配置键与默认值。
         *
         * 返回:
         * - `_MCP_FIELDS` 的深拷贝；字段 `path` 为组内相对路径（前缀 `mcp`）。
         */
        getMcpFields: function () {
            return JSON.parse(JSON.stringify(_MCP_FIELDS));
        },
        /**
         * 生成 MCP 挂载配置片段（纯函数，只读）。
         *
         * 参数:
         * - `info`: `{ command, dbPath }`（get_mcp_info 返回值的子集）。
         *
         * 返回:
         * - 片段数组：通用配置（JSON）+ DeepSeek Harness（YAML），
         *   结构见 `buildMcpClientSnippets`。
         */
        buildMcpClientSnippets: function (info) {
            return buildMcpClientSnippets(info);
        },
        destroy: function () {
            for (var i = 0; i < _unregisterFns.length; i++) {
                try { _unregisterFns[i](); } catch (_) { /* ignore */ }
            }
            _unregisterFns = [];
            for (var j = 0; j < _unsubs.length; j++) {
                try { _unsubs[j](); } catch (_) { /* ignore */ }
            }
            _unsubs = [];
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
