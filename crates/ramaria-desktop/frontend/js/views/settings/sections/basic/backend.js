/**
 * js/views/settings/sections/basic/backend.js — Ramaria 设置页「后端配置」目录项
 *
 * 职责:
 * - 后端连接参数（Provider / Base URL / Model ID / API Key）
 * - 图片理解能力声明（模型可识图开关 + 单批理解上限）
 *
 * 设计特点:
 * - 表单保存经 update_full_config 统一写入口双写
 * - API Key 遮蔽显示，输入框只用于填写新 key；图片理解上限随能力声明显隐
 *
 * 依赖:
 * - NS.$ / NS.handleUpdateFullResult（shared.js）
 * - RamariaApi / RamariaToast / RamariaStore
 */

(function (NS) {
    'use strict';

    var state = NS.state;

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

// ── 图片理解：模型能力声明（勾选后才出现单批理解上限）──
        var visionCard = document.createElement('div');
        visionCard.className = 'settings-card';
        visionCard.id = 'settings-vision-card';
        visionCard.innerHTML =
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">' +
                    '<input type="checkbox" id="settings-vision-enabled" /> ' +
                    '模型支持图片理解' +
                '</label>' +
                '<div class="settings-form-hint">' +
                    '显式声明当前对话模型可识图（线上模型会产生图片外发请求）；' +
                    '关闭时导入的图片整体跳过，仅保留 [图片#hash] 占位符。' +
                '</div>' +
            '</div>' +
            '<div class="settings-form-group hidden" id="settings-vision-batch-group">' +
                '<label class="settings-form-label">单批理解上限（张）</label>' +
                '<input class="settings-form-input" id="settings-vision-batch-limit" type="number" min="0" />' +
                '<div class="settings-form-hint">单次理解批次处理的图片数量上限；0 = 不限。</div>' +
            '</div>' +
            '<div class="settings-save-hint">' +
                '<button class="btn btn-primary btn-sm" id="settings-save-vision">保存图片理解设置</button>' +
            '</div>';
        section.appendChild(visionCard);

        parent.appendChild(section);

// 事件绑定
        _bindBackendEvents();
        _bindVisionEvents();
    }

    function _bindBackendEvents() {
// Provider 变化时调整 API Key 可见性和默认 URL
        var providerSelect = NS.$('settings-provider');
        var apiKeyGroup = NS.$('settings-api-key-group');
        var baseUrlInput = NS.$('settings-base-url');

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
                if (!baseUrlInput.value || baseUrlInput.value === _getDefaultUrl(state.backendConfig ? state.backendConfig.provider : '')) {
                    baseUrlInput.value = _getDefaultUrl(this.value);
                }
            });
        }

 // 保存按钮
        var saveBtn = NS.$('settings-save-backend');
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
        var providerEl = NS.$('settings-provider');
        var baseUrlEl = NS.$('settings-base-url');
        var modelIdEl = NS.$('settings-model-id');
        var apiKeyHint = NS.$('settings-api-key-hint');
        var apiKeyGroup = NS.$('settings-api-key-group');

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
        var provider = (NS.$('settings-provider') || {}).value;
        var baseUrl = (NS.$('settings-base-url') || {}).value;
        var modelId = (NS.$('settings-model-id') || {}).value;
        var apiKey = (NS.$('settings-api-key') || {}).value;

        if (!provider || !baseUrl) {
            RamariaToast.show('warning', '请填写 Provider 和 Base URL');
            return;
        }

 // 线上 API Key 检查
        if (provider !== 'lm_studio' && (!apiKey || apiKey.trim().length < 5)) {
            RamariaToast.show('warning', '线上后端需要提供 API Key');
            return;
        }

        var saveBtn = NS.$('settings-save-backend');
        if (saveBtn) {
            saveBtn.disabled = true;
            saveBtn.textContent = '保存中...';
        }

        try {
            await RamariaApi.config.updateBackend(provider, modelId || '', baseUrl, apiKey || '');
            RamariaToast.show('success', '后端配置已保存');

 // 刷新本地缓存
            state.backendConfig = await RamariaApi.config.getBackend();
            RamariaStore.set('backendConfig', state.backendConfig);

 // 清空 API Key 输入
            var keyInput = NS.$('settings-api-key');
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
// 图片理解（后端能力声明）
// =========================================================

    /**
     * 「图片理解」字段元数据（默认值与类型）。
     *
     * 字段约定:
     * - `path`: 组内相对路径；完整配置路径 = ['vision'].concat(path)。
     * - `def`: 默认值，与 `config/default.toml` 的 `[vision]` 组逐键一致（回归测试锁定）。
     *
     * 说明:
     * - 界面控件按此元数据维护默认值与文案；本数组供默认值一致性回归读取（不参与渲染）。
     */
    var _VISION_FIELDS = [
        { path: ['model_supports_vision'], type: 'bool', def: false },
        { path: ['batch_limit'], type: 'number', min: 0, def: 0 },
    ];

    /**
     * 绑定图片理解区块事件。
     *
     * 交互:
     * - 勾选「模型支持图片理解」后才显示「单批理解上限」输入框（与既有高级设置控件形态一致）；
     * - 保存经 `update_full_config` 统一写入口双写（与高级设置的同一组配置键）。
     */
    function _bindVisionEvents() {
        var enabledBox = NS.$('settings-vision-enabled');
        if (enabledBox) {
            enabledBox.addEventListener('change', _syncVisionBatchVisibility);
        }
        var saveBtn = NS.$('settings-save-vision');
        if (saveBtn) {
            saveBtn.addEventListener('click', _handleSaveVision);
        }
    }

    /**
     * 单批理解上限输入组随能力声明勾选状态显示 / 隐藏。
     */
    function _syncVisionBatchVisibility() {
        var enabledBox = NS.$('settings-vision-enabled');
        var batchGroup = NS.$('settings-vision-batch-group');
        if (!enabledBox || !batchGroup) return;
        batchGroup.classList.toggle('hidden', !enabledBox.checked);
    }

    /**
     * 回显图片理解配置（能力声明 + 单批上限）。
     */
    function _fillVisionForm(config) {
        if (!config || !config.vision) return;

        var enabledBox = NS.$('settings-vision-enabled');
        var batchInput = NS.$('settings-vision-batch-limit');
        if (enabledBox) enabledBox.checked = config.vision.model_supports_vision === true;
        if (batchInput) batchInput.value = config.vision.batch_limit;

        _syncVisionBatchVisibility();
    }

    /**
     * 收集并校验图片理解表单值。
     *
     * 校验:
     * - 单批上限为 ≥ 0 的整数（留空按 0 = 不限）；
     * - 能力声明关闭时不校验输入框（上限值不参与写入，重新开启后沿用原值）。
     *
     * 返回:
     * - `{ enabled, batchLimit }`（校验通过；`batchLimit` 仅能力开启时有意义）；
     * - `{ error }`（校验失败，调用方提示）。
     */
    function _collectVisionValues() {
        var enabledBox = NS.$('settings-vision-enabled');
        var batchInput = NS.$('settings-vision-batch-limit');
        var enabled = !!(enabledBox && enabledBox.checked);

        var batchLimit = 0;
        var raw = batchInput ? String(batchInput.value).trim() : '';
        if (enabled && raw !== '') {
            batchLimit = parseInt(raw, 10);
            if (isNaN(batchLimit) || batchLimit < 0) {
                return { error: '单批理解上限需为 ≥ 0 的整数（0 = 不限）' };
            }
        }
        return { enabled: enabled, batchLimit: batchLimit };
    }

    /**
     * 把图片理解表单值合并进完整配置（纯函数：不修改入参，返回新配置）。
     *
     * 说明:
     * - 能力声明关闭时只关声明、保留已配置的单批上限（重新开启后沿用）。
     */
    function _applyVisionFormToConfig(config, values) {
        var cfg = JSON.parse(JSON.stringify(config));
        if (!cfg.vision) cfg.vision = {};
        cfg.vision.model_supports_vision = !!values.enabled;
        if (values.enabled) cfg.vision.batch_limit = values.batchLimit;
        return cfg;
    }

    /**
     * 保存图片理解配置（能力声明 + 单批上限）。
     */
    async function _handleSaveVision() {
        try {
            if (!state.fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }

            var collected = _collectVisionValues();
            if (collected.error) {
                RamariaToast.show('warning', collected.error);
                return;
            }

            var cfg = _applyVisionFormToConfig(state.fullConfig, collected);
            var result = await RamariaApi.config.updateFull(cfg);
            if (!NS.handleUpdateFullResult(result, '图片理解设置已保存')) {
                throw new Error('配置双写均失败');
            }
            state.fullConfig = cfg;
        } catch (err) {
            RamariaToast.show('error', '保存失败', err.message || '未知错误');
        }
    }

 // =========================================================
 // 挂载面
 // =========================================================

    NS.parts.backend = {
        /** 渲染后端配置目录项（后端连接参数 + 图片理解卡片） */
        render: _renderBackendSection,
        /** 回显后端连接配置 */
        fill: _fillBackendForm,
        /** 回显图片理解配置（能力声明 + 单批上限） */
        fillVision: _fillVisionForm,
        /** 单批上限输入组显隐同步（供回归测试直接驱动） */
        syncVisionBatchVisibility: _syncVisionBatchVisibility,
        /** 图片理解表单值收集与校验（纯读取，供回归测试直接驱动） */
        collectVisionValues: _collectVisionValues,
        /** 图片理解表单值合并完整配置（纯函数，供回归测试直接驱动） */
        applyVisionFormToConfig: _applyVisionFormToConfig,
        /** 图片理解字段元数据（默认值一致性回归读取） */
        visionFields: _VISION_FIELDS,
    };
})(window.RamariaSettingsNS);
