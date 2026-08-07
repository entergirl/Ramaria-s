/**
 * js/views/settings.js — Ramaria 设置页视图
 *
 * 职责:
 * - 后端配置（Provider / Base URL / Model ID / API Key）
 * - 隐私设置（隐私确认状态查看 / 记忆注入开关）
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

 // =========================================================
 // DOM 快捷查询
 // =========================================================

    function $(id) { return document.getElementById(id); }

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

 // ── v1.4 M6（T-V14-6-005）：基础/高级两级 Tab 框架 ──
        var tabs = document.createElement('div');
        tabs.className = 'settings-tabs';
        tabs.innerHTML =
            '<button class="settings-tab-btn active" data-tab="basic">基础设置</button>' +
            '<button class="settings-tab-btn" data-tab="advanced">高级设置</button>';
        scroll.appendChild(tabs);

        var basicPane = document.createElement('div');
        basicPane.className = 'settings-tab-pane';
        basicPane.id = 'settings-pane-basic';
        scroll.appendChild(basicPane);

        var advancedPane = document.createElement('div');
        advancedPane.className = 'settings-tab-pane hidden';
        advancedPane.id = 'settings-pane-advanced';
        scroll.appendChild(advancedPane);

 // ── 基础设置（面向日常用户，T-V14-6-005）──
        _renderBackendSection(basicPane);
        _renderEmbeddingSection(basicPane);
        _renderMemoryInjectionSection(basicPane);
        _renderSessionSection(basicPane);
        _renderPrivacySection(basicPane);
        _renderDataDirSection(basicPane);
        _renderDataSection(basicPane);
        _renderDiagnosticsSection(basicPane);
        _renderAboutSection(basicPane);

 // ── 高级设置（面向进阶用户与排障，T-V14-6-006）──
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
            '<div class="settings-form-group" id="settings-api-key-group">' +
                '<label class="settings-form-label">API Key</label>' +
                '<input class="settings-form-input" id="settings-api-key" type="password" ' +
                    'placeholder="填入新 key 以更换，留空保持不变" />' +
                '<div class="settings-form-hint">密钥存储于系统凭证管理器。当前 key：<span id="settings-api-key-hint" class="font-mono">加载中...</span></div>' +
            '</div>' +
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
 // 记忆注入开关区块（v1.4 M6，T-V14-6-005 基础设置）
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
            if (result && result.fileOk === false && result.dbOk === false) {
                throw new Error('配置双写均失败');
            }
            _fullConfig = cfg;
            RamariaToast.show('success', '记忆注入设置已保存');
        } catch (err) {
            RamariaToast.show('error', '保存失败', err.message || '未知错误');
        }
    }

 // =========================================================
 // 数据目录区块（v1.4 M6，T-V14-6-005 基础设置）
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
            if (result && result.fileOk === false && result.dbOk === false) {
                throw new Error('配置双写均失败');
            }
            _fullConfig = cfg;
            RamariaToast.show('success', '数据目录已保存（重启后生效）');
        } catch (err) {
            RamariaToast.show('error', '保存失败', err.message || '未知错误');
        }
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
            '<div id="settings-update-detail" class="hidden" style="margin-top:8px;">' +
                '<div class="settings-row-meta" id="settings-update-message"></div>' +
            '</div>' +
            '<div class="settings-actions" style="margin-top:12px;">' +
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
 // 多行错误消息转为带换行的 HTML（安全：后端错误消息不含用户输入）
                    msgEl.innerHTML = result.error.replace(/\n/g, '<br>');
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
                    var releaseHtml = '发现新版本: <strong>' + (result.latestVersion || '?') + '</strong>';
                    if (result.releaseUrl) {
                        releaseHtml += ' — <a href="' + result.releaseUrl + '" target="_blank" rel="noopener" class="settings-about-link">前往下载</a>';
                    }
                    if (result.releaseNotesPreview) {
                        releaseHtml += '<br><small class="text-tertiary">' +
                            result.releaseNotesPreview.replace(/\n/g, '<br>') + '</small>';
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
                msgEl.innerHTML = errText.replace(/\n/g, '<br>');
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
            '<div class="settings-about-version" id="settings-about-version">v1.2.0</div>' +
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
 // 高级设置（v1.4 M6，T-V14-6-006）
 // =========================================================

    /**
     * 高级配置组元数据（字段默认值与 ramaria-core/src/config.rs 的 Default 实现对齐）。
     *
     * 字段约定:
     * - `path`: 配置 JSON 中的字段路径（数组，支持嵌套）。
     * - `type`: `number` | `bool` | `whitelist`。
     * - `def`: 默认值（恢复默认与默认值标注用；`whitelist` 为数组）。
     * - `options`: 仅 `whitelist` 使用（可选值列表 {value, label}）。
     */
    var _ADVANCED_GROUPS = [
        {
            key: 'retrieval',
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
            ],
        },
        {
            key: 'decay',
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
            title: '🎚️ 记忆层触发阈值',
            desc: '控制 L2 合并与 L3 推断的触发条件（计数 + 时间双路径）。',
            fields: [
                { path: ['l2_trigger_count'], label: 'L2 触发条数', type: 'number', min: 1, def: 5, hint: '未吸收 L1 达到此条数触发 L2 合并' },
                { path: ['l2_trigger_days'], label: 'L2 触发天数', type: 'number', min: 1, def: 7, hint: '最早未吸收 L1 超过此天数触发 L2' },
                { path: ['l3_trigger_count'], label: 'L3 触发条数', type: 'number', min: 1, def: 10, hint: '未吸收事件达到此条数触发 L3 推断' },
                { path: ['l3_trigger_days'], label: 'L3 触发天数', type: 'number', min: 1, def: 30, hint: '最早未吸收事件超过此天数触发 L3' },
                { path: ['cluster_delay_ms'], label: '簇间延迟（毫秒）', type: 'number', min: 0, def: 800, hint: 'L2 事件提取簇间请求间隔，避免速率限制（DeepSeek 建议调大）' },
            ],
        },
        {
            key: 'index',
            title: '🗂️ 索引与 BM25',
            desc: 'BM25 增量合并与周期性重建节奏。',
            fields: [
                { path: ['bm25_incremental_threshold'], label: '增量合并阈值', type: 'number', min: 1, def: 10, hint: '缓冲区积累超过此条数触发合并' },
                { path: ['bm25_rebuild_interval'], label: '重建间隔（秒）', type: 'number', min: 10, def: 300, hint: 'BM25 定时重建检查间隔' },
            ],
        },
        {
            key: 'logging',
            title: '📜 日志',
            desc: '日志记录级别控制。',
            fields: [
                { path: ['log_full_prompt'], label: '记录完整 Prompt', type: 'bool', def: false, hint: '记录完整 prompt 含记忆上下文与原文片段（隐私敏感，开启需弹窗确认）' },
            ],
        },
        {
            key: 'inference',
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
            title: '📇 事件提取',
            desc: 'L1→L2 事件提取器的 LLM 参数（独立于对话参数，JSON 输出需更大 token 预算）。',
            fields: [
                { path: ['temperature'], label: '提取温度', type: 'number', step: 0.1, min: 0, max: 2, def: 0.3, hint: '事件提取 LLM 温度' },
                { path: ['max_tokens'], label: '最大输出 tokens', type: 'number', min: 256, def: 8192, hint: '事件 JSON 输出预算' },
                { path: ['max_events'], label: '单簇最大事件数', type: 'number', min: 1, def: 5, hint: '单簇最多提取的事件数' },
            ],
        },
        {
            key: 'utt',
            title: '💬 utt 原文通道',
            desc: '原文话语块切分、检索与注入参数。原文是最高敏感层，白名单外 persona 不注入。',
            fields: [
                { path: ['enabled'], label: '启用原文通道', type: 'bool', def: true, hint: '关闭后行为回退 v1.3（不注入原文片段）' },
                { path: ['theta_gap_minutes'], label: '切分时间间隙（分钟）', type: 'number', min: 1, def: 30, hint: '相邻消息间隔超过此值切分为新块' },
                { path: ['max_msgs_per_block'], label: '单块最大消息数', type: 'number', min: 1, def: 40, hint: '超过此条数强制切分' },
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
            title: '🎭 示例注入',
            desc: 'Few-shot 示例的自学习抽取、评分轮换与兜底注入参数。',
            fields: [
                { path: ['enabled'], label: '启用示例注入', type: 'bool', def: true, hint: '关闭后回退 v1.3 静态 selected 查询' },
                { path: ['max_examples'], label: '最大示例条数', type: 'number', min: 1, def: 5, hint: '注入时的最大示例条数' },
            ],
        },
        {
            key: 'bridge',
            title: '🌉 会话桥接',
            desc: '新会话加载上一会话尾部原文，保持对话连贯性。',
            fields: [
                { path: ['enabled'], label: '启用桥接', type: 'bool', def: true, hint: '关闭后新会话不加载桥接（等同 v1.3）' },
                { path: ['max_chars'], label: '桥接字符预算', type: 'number', min: 50, def: 800, hint: '超限从头部截断、保最近' },
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
            '非排障场景请保持默认值；每项均可通过「恢复默认」还原。';
        parent.appendChild(risk);

        for (var i = 0; i < _ADVANCED_GROUPS.length; i++) {
            _renderAdvancedGroup(parent, _ADVANCED_GROUPS[i]);
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
                html += '<div class="settings-form-group">' +
                    '<label class="settings-form-label">' +
                        '<input type="checkbox" id="' + fid + '" /> ' + f.label +
                    '</label>' +
                    '<div class="settings-form-hint">' + f.hint + '（默认：' + (f.def ? '开' : '关') + '）</div>' +
                '</div>';
            } else if (f.type === 'whitelist') {
                html += '<div class="settings-form-group">' +
                    '<label class="settings-form-label">' + f.label + '</label>' +
                    '<div class="settings-form-whitelist">';
                for (var w = 0; w < f.options.length; w++) {
                    var opt = f.options[w];
                    html += '<label class="settings-form-inline-label">' +
                        '<input type="checkbox" id="' + fid + '-' + opt.value + '" data-value="' + opt.value + '" /> ' +
                        opt.label +
                    '</label>';
                }
                html += '</div>' +
                    '<div class="settings-form-hint">' + f.hint + '（默认：' + f.def.join(', ') + '）</div>' +
                '</div>';
            } else {
                html += '<div class="settings-form-group">' +
                    '<label class="settings-form-label">' + f.label + '</label>' +
                    '<input class="settings-form-input" id="' + fid + '" type="number"' +
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

        // log_full_prompt 开启需显式隐私确认（T-V14-6-006）
        var logBox = $('settings-adv-logging-log_full_prompt');
        if (logBox) {
            logBox.addEventListener('change', function () {
                if (!logBox.checked) return;
                RamariaModal.show({
                    title: '⚠️ 隐私确认',
                    body: '开启后将把完整 prompt（含记忆上下文与原文片段）写入日志，' +
                        '可能包含敏感信息。仅在排障时短期开启，用后请立即关闭。',
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
     */
    function _advSetValue(cfg, path, value) {
        var v = cfg;
        for (var i = 0; i < path.length - 1; i++) {
            v = v[path[i]];
        }
        v[path[path.length - 1]] = value;
    }

    /**
     * 回显一个高级配置组（进入设置页时调用）。
     */
    function _fillAdvancedForm(group, cfg) {
        for (var i = 0; i < group.fields.length; i++) {
            var f = group.fields[i];
            var fid = _advFieldId(group, f);
            var value = _advGetValue(cfg, f.path);
            if (f.type === 'bool') {
                var box = $(fid);
                if (box) box.checked = !!value;
            } else if (f.type === 'whitelist') {
                var list = Array.isArray(value) ? value : [];
                for (var w = 0; w < f.options.length; w++) {
                    var cb = $(fid + '-' + f.options[w].value);
                    if (cb) cb.checked = list.indexOf(f.options[w].value) !== -1;
                }
            } else {
                var input = $(fid);
                if (input && value !== undefined) input.value = value;
            }
        }
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
            }
            entries.push({ path: f.path, value: value });
        }
        return entries;
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
            if (result && result.fileOk === false && result.dbOk === false) {
                throw new Error('配置双写均失败');
            }
            _fullConfig = cfg;
            RamariaToast.show('success', group.title + ' 已保存');
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
                _advSetValue(cfg, f.path, JSON.parse(JSON.stringify(f.def)));
            }

            var result = await RamariaApi.config.updateFull(cfg);
            if (result && result.fileOk === false && result.dbOk === false) {
                throw new Error('配置双写均失败');
            }
            _fullConfig = cfg;
            _fillAdvancedForm(group, cfg);
            RamariaToast.show('success', group.title + ' 已恢复默认并保存');
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

 // 加载完整配置并回显（v1.4 M5 会话区块 + M6 基础/高级表单）
            try {
                _fullConfig = await RamariaApi.config.getFull();
                _fillSessionForm(_fullConfig);
                _fillMemoryInjectionForm(_fullConfig);
                _fillDataDirForm(_fullConfig);
                _fillAdvancedForms(_fullConfig);
            } catch (err) {
                console.error('[SettingsView] 加载完整配置失败:', err);
            }

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
