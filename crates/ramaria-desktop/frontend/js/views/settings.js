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

 // ── 后端配置 ──
        _renderBackendSection(scroll);

 // ── 嵌入模型配置──
        _renderEmbeddingSection(scroll);

 // ── 隐私设置 ──
        _renderPrivacySection(scroll);

 // ── 数据管理 ──
        _renderDataSection(scroll);

 // ── 诊断与更新──
        _renderDiagnosticsSection(scroll);

 // ── 关于 ──
        _renderAboutSection(scroll);
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
            '<div class="settings-about-version" id="settings-about-version">v1.1.0</div>' +
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
