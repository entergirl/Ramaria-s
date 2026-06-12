/**
 * js/views/setup.js — Ramaria 首次配置向导
 *
 * 设计参考: Python static/setup.html
 *
 * 职责:
 * - 三步配置：LLM 模型配置 → 确认 → 完成并启动
 * - 模式切换：本地（LM Studio） / 线上（DeepSeek/OpenAI）
 * - 连接测试：测试按钮 + dot 状态指示
 * - 注册 Router enter/leave 钩子
 *
 * 设计特点:
 * - 居中卡片布局（540px max），毛玻璃背景
 * - 步骤指示器：圆点 + 连接线 + 标签在下方
 * - mono 字体表单，coral 聚焦环
 * - 本地模式：填 base_url + model_name
 * - 线上模式：填 api_key + base_url + model_name
 * - 测试通过才能继续
 *
 * 依赖:
 * - RamariaApi / RamariaStore / RamariaRouter
 * - RamariaToast / RamariaModal
 * - CSS: css/views/setup.css
 */
var RamariaSetupView = (function () {
    'use strict';

    // =========================================================
    // 常量
    // =========================================================

    var STEP_LABELS = ['对话模型', '确认信息', '完成'];
    var TOTAL_STEPS = 3;

    // =========================================================
    // 内部状态
    // =========================================================

    var _unregisterFns = [];
    var _currentStep = 1;
    var _currentMode = 'local'; // 'local' | 'api'
    var _testPassed = false;
    var _submitting = false;

    function $(id) { return document.getElementById(id); }

    // =========================================================
    // 渲染
    // =========================================================

    function render() {
        var container = $('view-setup');
        if (!container) return;

        _currentStep = 1;
        _currentMode = 'local';
        _testPassed = false;
        _submitting = false;

        container.innerHTML = '';

        // ── 全屏滚动容器 ──
        var wizard = document.createElement('div');
        wizard.className = 'setup-wizard';
        container.appendChild(wizard);

        // ── 顶部品牌 ──
        var brand = document.createElement('div');
        brand.className = 'setup-top-brand';
        brand.innerHTML =
            '<span class="setup-brand-icon">🪸</span>' +
            '<span class="setup-brand-name">珊瑚菌 · 配置向导</span>';
        wizard.appendChild(brand);

        // ── 步骤指示器（占位，后续动态渲染）──
        var stepper = document.createElement('div');
        stepper.className = 'setup-stepper';
        stepper.id = 'setup-stepper';
        wizard.appendChild(stepper);

        // ── 主卡片 ──
        var card = document.createElement('div');
        card.className = 'setup-card';

        // 面板 1：LLM 模型配置
        card.innerHTML +=
            '<div class="setup-panel active" id="setup-panel-1">' +
                _panel1Html() +
            '</div>';

        // 面板 2：确认
        card.innerHTML +=
            '<div class="setup-panel" id="setup-panel-2">' +
                _panel2Html() +
            '</div>';

        // 面板 3：完成
        card.innerHTML +=
            '<div class="setup-panel" id="setup-panel-3">' +
                _panel3Html() +
            '</div>';

        // ── 底部操作区 ──
        card.innerHTML +=
            '<div class="setup-card-footer">' +
                '<button class="setup-btn setup-btn-prev" id="setup-btn-prev" style="visibility:hidden">← 上一步</button>' +
                '<span class="setup-step-indicator" id="setup-step-indicator">1 / ' + TOTAL_STEPS + '</span>' +
                '<button class="setup-btn setup-btn-next" id="setup-btn-next">下一步 →</button>' +
            '</div>';

        wizard.appendChild(card);

        // 绑定事件
        _bindEvents();
        _renderStepper();
        _showStep(1);
    }

    // =========================================================
    // 面板 HTML
    // =========================================================

    function _panel1Html() {
        return '' +
            '<div class="setup-panel-title">对话模型配置</div>' +
            '<div class="setup-panel-desc">' +
                '选择珊瑚菌对话模型的运行方式。<br>' +
                '嵌入模型（向量检索）始终本地运行，不受此选择影响。' +
            '</div>' +

            // 模式选择器
            '<div class="setup-mode-selector" id="setup-mode-selector">' +
                '<button class="setup-mode-option active" data-mode="local">' +
                    '<span class="setup-mode-option-icon">🖥</span>' +
                    '<span class="setup-mode-option-title">本地部署</span>' +
                    '<span class="setup-mode-option-desc">隐私保护 · 全量本地运行</span>' +
                '</button>' +
                '<button class="setup-mode-option" data-mode="api">' +
                    '<span class="setup-mode-option-icon">☁</span>' +
                    '<span class="setup-mode-option-title">线上 API</span>' +
                    '<span class="setup-mode-option-desc">快速启动 · 无硬件负担</span>' +
                '</button>' +
            '</div>' +

            // 本地字段
            '<div class="setup-field-group" id="setup-local-fields">' +
                '<div class="setup-field">' +
                    '<div class="setup-field-label">推理服务地址 <span class="setup-required">*</span></div>' +
                    '<input class="setup-field-input" id="setup-local-url" type="text" ' +
                        'placeholder="http://localhost:1234/v1" autocomplete="off">' +
                    '<div class="setup-field-hint">兼容 OpenAI API 格式的本地推理服务地址（LM Studio 默认端口 1234）</div>' +
                '</div>' +
                '<div class="setup-field">' +
                    '<div class="setup-field-label">模型名称 <span class="setup-required">*</span></div>' +
                    '<input class="setup-field-input" id="setup-local-model" type="text" ' +
                        'placeholder="例如 qwen/qwen3.5-9b" autocomplete="off">' +
                    '<div class="setup-field-hint">必须与推理服务中实际加载的模型名称一致（区分大小写）</div>' +
                '</div>' +
                '<div class="setup-service-hint">' +
                    '<strong>LM Studio</strong>：打开 → Local Server → 选择模型 → Start Server<br>' +
                    '<strong>Ollama</strong>：运行 <code>ollama serve</code>' +
                '</div>' +
            '</div>' +

            // 线上字段
            '<div class="setup-field-group" id="setup-api-fields" style="display:none">' +
                '<div class="setup-field">' +
                    '<div class="setup-field-label">API Key <span class="setup-required">*</span></div>' +
                    '<input class="setup-field-input" id="setup-api-key" type="password" ' +
                        'placeholder="sk-xxxxxxxxxxxxxxxx" autocomplete="off">' +
                    '<div class="setup-field-hint">从对应服务商获取（DeepSeek、OpenAI）</div>' +
                '</div>' +
                '<div class="setup-field">' +
                    '<div class="setup-field-label">API 地址 <span class="setup-required">*</span></div>' +
                    '<input class="setup-field-input" id="setup-api-url" type="text" ' +
                        'placeholder="https://api.deepseek.com/v1" autocomplete="off">' +
                    '<div class="setup-field-hint">常用：DeepSeek（api.deepseek.com）、OpenAI（api.openai.com）</div>' +
                '</div>' +
                '<div class="setup-field">' +
                    '<div class="setup-field-label">模型名称 <span class="setup-required">*</span></div>' +
                    '<input class="setup-field-input" id="setup-api-model" type="text" ' +
                        'placeholder="deepseek-chat" autocomplete="off">' +
                    '<div class="setup-field-hint">必须与 API 服务商的模型标识符一致。如 deepseek-chat、gpt-4o</div>' +
                '</div>' +
                '<div class="setup-privacy-hint">' +
                    '<strong>隐私提示：</strong>对话内容（含记忆上下文）将发送给线上 API 服务商处理。<br>' +
                    '敏感对话建议使用本地部署模式。' +
                '</div>' +
            '</div>' +

            // 连接测试按钮
            '<div style="margin-top:12px">' +
                '<button class="setup-test-btn" id="setup-test-btn">' +
                    '<span class="setup-test-dot"></span> 测试连接' +
                '</button>' +
                '<div class="setup-field-status" id="setup-test-status"></div>' +
            '</div>';
    }

    function _panel2Html() {
        return '' +
            '<div class="setup-panel-title">配置确认</div>' +
            '<div class="setup-panel-desc">请核对以下配置信息，无误后点击「完成并启动」</div>' +
            '<div class="setup-summary-box" id="setup-summary-box">' +
                // 由 JS 填充
            '</div>';
    }

    function _panel3Html() {
        return '' +
            '<div class="setup-finish-icon">✓</div>' +
            '<div class="setup-finish-title">配置完成</div>' +
            '<div class="setup-finish-desc">' +
                '所有必要配置已填写完毕。<br>' +
                '点击「进入 Ramaria」将保存配置并进入对话界面。' +
            '</div>' +
            '<div class="setup-init-status" id="setup-init-status">' +
                '<div class="setup-init-line" id="setup-init-save">' +
                    '<span class="setup-mark-wait">⏳</span> 保存配置中…' +
                '</div>' +
                '<div class="setup-init-line" id="setup-init-done" style="display:none">' +
                    '<span class="setup-mark-ok">✓</span> 完成，正在进入对话界面…' +
                '</div>' +
            '</div>';
    }

    // =========================================================
    // 步骤指示器
    // =========================================================

    function _renderStepper() {
        var container = $('setup-stepper');
        if (!container) return;
        container.innerHTML = '';

        for (var i = 1; i <= TOTAL_STEPS; i++) {
            var item = document.createElement('div');
            item.className = 'setup-step-item';
            if (i === _currentStep) item.classList.add('active');

            var circle = document.createElement('div');
            circle.className = 'setup-step-circle';
            if (i < _currentStep) {
                circle.classList.add('done');
                circle.textContent = '✓';
            } else if (i === _currentStep) {
                circle.classList.add('active');
                circle.textContent = String(i);
            } else {
                circle.textContent = String(i);
            }

            var label = document.createElement('span');
            label.className = 'setup-step-label';
            label.textContent = STEP_LABELS[i - 1];

            item.appendChild(circle);
            item.appendChild(label);
            container.appendChild(item);

            if (i < TOTAL_STEPS) {
                var line = document.createElement('div');
                line.className = 'setup-step-line';
                if (i < _currentStep) line.classList.add('done');
                container.appendChild(line);
            }
        }
    }

    // =========================================================
    // 面板切换
    // =========================================================

    function _showStep(step) {
        _currentStep = step;

        // 切换面板
        var panels = document.querySelectorAll('.setup-panel');
        for (var i = 0; i < panels.length; i++) {
            panels[i].classList.toggle('active', panels[i].id === 'setup-panel-' + step);
        }

        // 更新导航按钮
        var btnPrev = $('setup-btn-prev');
        var btnNext = $('setup-btn-next');
        var indicator = $('setup-step-indicator');

        if (btnPrev) btnPrev.style.visibility = step > 1 ? 'visible' : 'hidden';

        if (btnNext) {
            if (step === TOTAL_STEPS) {
                btnNext.textContent = '进入 Ramaria';
            } else if (step === TOTAL_STEPS - 1) {
                btnNext.textContent = '完成并启动';
            } else {
                btnNext.textContent = '下一步 →';
            }
        }

        if (indicator) indicator.textContent = step + ' / ' + TOTAL_STEPS;

        // 进入确认步骤时填充摘要
        if (step === TOTAL_STEPS - 1) _fillSummary();

        _renderStepper();
    }

    // =========================================================
    // 事件绑定
    // =========================================================

    function _bindEvents() {
        // 模式切换
        var modeOptions = document.querySelectorAll('.setup-mode-option');
        for (var i = 0; i < modeOptions.length; i++) {
            modeOptions[i].addEventListener('click', function () {
                _selectMode(this.getAttribute('data-mode'));
            });
        }

        // 上一步 / 下一步
        var btnPrev = $('setup-btn-prev');
        var btnNext = $('setup-btn-next');
        if (btnPrev) btnPrev.addEventListener('click', _goPrev);
        if (btnNext) btnNext.addEventListener('click', _goNext);

        // 测试连接
        var testBtn = $('setup-test-btn');
        if (testBtn) testBtn.addEventListener('click', _testConnection);

        // 键盘 Enter 在表单中触发下一步
        document.addEventListener('keydown', function (e) {
            if (e.key === 'Enter' && e.target && e.target.closest('.setup-field-input')) {
                e.preventDefault();
                _goNext();
            }
        });
    }

    function _selectMode(mode) {
        _currentMode = mode;
        _testPassed = false;

        // 更新按钮选中态
        var options = document.querySelectorAll('.setup-mode-option');
        for (var i = 0; i < options.length; i++) {
            options[i].classList.toggle('active', options[i].getAttribute('data-mode') === mode);
        }

        // 切换字段组
        var localFields = $('setup-local-fields');
        var apiFields = $('setup-api-fields');
        if (localFields) localFields.style.display = mode === 'local' ? '' : 'none';
        if (apiFields) apiFields.style.display = mode === 'api' ? '' : 'none';

        // 重置测试状态
        _resetTestState();
    }

    function _resetTestState() {
        var btn = $('setup-test-btn');
        var status = $('setup-test-status');

        if (btn) {
            btn.className = 'setup-test-btn';
            btn.innerHTML = '<span class="setup-test-dot"></span> 测试连接';
        }
        if (status) {
            status.textContent = '';
            status.className = 'setup-field-status';
        }
        _testPassed = false;

        // 清除输入框高亮
        var inputs = document.querySelectorAll('.setup-field-input.valid, .setup-field-input.invalid');
        for (var i = 0; i < inputs.length; i++) {
            inputs[i].classList.remove('valid', 'invalid');
        }
    }

    // =========================================================
    // 测试连接
    // =========================================================

    async function _testConnection() {
        var btn = $('setup-test-btn');
        var status = $('setup-test-status');
        if (!btn || !status) return;

        // 校验必填字段
        var valid = _validateModeFields(true);
        if (!valid) return;

        btn.className = 'setup-test-btn testing';
        btn.innerHTML = '<span class="setup-test-dot"></span> 测试中…';
        status.textContent = '';
        status.className = 'setup-field-status checking';

        try {
            // 先保存临时配置
            var config = _collectConfig();
            await RamariaApi.config.updateBackend(
                config.provider,
                config.modelId,
                config.baseUrl,
                _currentMode === 'api' ? config.apiKey : ''
            );

            // 调用 refresh 来验证
            var newState = await RamariaApi.setup.refresh();

            // 检查状态
            if (newState === 'ready' || newState === 'downloading_model' || newState === 'indexing') {
                // LLM 连接正常
                btn.className = 'setup-test-btn ok';
                btn.innerHTML = '<span class="setup-test-dot"></span> 连接成功';
                status.textContent = '✓ ' + (_currentMode === 'api' ? '线上 API' : '本地推理服务') + ' 可达';
                status.className = 'setup-field-status ok';
                _testPassed = true;

                // 高亮输入框
                _highlightInputs(true);
            } else if (newState === 'degraded') {
                btn.className = 'setup-test-btn fail';
                btn.innerHTML = '<span class="setup-test-dot"></span> 连接失败';
                status.textContent = '✗ 服务连接异常，请检查配置';
                status.className = 'setup-field-status fail';
                _testPassed = false;
            } else {
                btn.className = 'setup-test-btn fail';
                btn.innerHTML = '<span class="setup-test-dot"></span> 测试失败';
                status.textContent = '✗ 无法确定服务状态: ' + newState;
                status.className = 'setup-field-status fail';
                _testPassed = false;
            }
        } catch (err) {
            var msg = err.message || String(err);
            btn.className = 'setup-test-btn fail';
            btn.innerHTML = '<span class="setup-test-dot"></span> 测试失败';
            status.textContent = '✗ ' + msg;
            status.className = 'setup-field-status fail';
            _testPassed = false;
        }
    }

    function _highlightInputs(ok) {
        var cls = ok ? 'valid' : 'invalid';
        var inputs = document.querySelectorAll(
            _currentMode === 'local'
                ? '#setup-local-fields .setup-field-input'
                : '#setup-api-fields .setup-field-input'
        );
        for (var i = 0; i < inputs.length; i++) {
            inputs[i].classList.add(cls);
        }
    }

    // =========================================================
    // 表单校验
    // =========================================================

    function _validateModeFields(showToast) {
        if (_currentMode === 'local') {
            var url = ($('setup-local-url') || {}).value || '';
            var model = ($('setup-local-model') || {}).value || '';

            if (!url.trim()) {
                if (showToast) RamariaToast.show('error', '请填写推理服务地址');
                return false;
            }
            if (!url.trim().startsWith('http')) {
                if (showToast) RamariaToast.show('error', '服务地址格式不正确，应以 http:// 开头');
                return false;
            }
            if (!model.trim()) {
                if (showToast) RamariaToast.show('error', '请填写模型名称');
                return false;
            }
        } else {
            var apiKey = ($('setup-api-key') || {}).value || '';
            var apiUrl = ($('setup-api-url') || {}).value || '';
            var apiModel = ($('setup-api-model') || {}).value || '';

            if (!apiKey.trim()) {
                if (showToast) RamariaToast.show('error', '请填写 API Key');
                return false;
            }
            if (!apiUrl.trim()) {
                if (showToast) RamariaToast.show('error', '请填写 API 地址');
                return false;
            }
            if (!apiUrl.trim().startsWith('http')) {
                if (showToast) RamariaToast.show('error', 'API 地址格式不正确，应以 http 开头');
                return false;
            }
            if (!apiModel.trim()) {
                if (showToast) RamariaToast.show('error', '请填写模型名称');
                return false;
            }
        }
        return true;
    }

    function _collectConfig() {
        if (_currentMode === 'local') {
            return {
                provider: 'LmStudio',
                baseUrl: ($('setup-local-url') || {}).value || 'http://localhost:1234/v1',
                modelId: ($('setup-local-model') || {}).value || '',
                apiKey: '',
            };
        } else {
            return {
                provider: 'DeepSeek',
                baseUrl: ($('setup-api-url') || {}).value || 'https://api.deepseek.com/v1',
                modelId: ($('setup-api-model') || {}).value || '',
                apiKey: ($('setup-api-key') || {}).value || '',
            };
        }
    }

    // =========================================================
    // 导航
    // =========================================================

    function _goPrev() {
        if (_currentStep > 1) {
            _showStep(_currentStep - 1);
        }
    }

    async function _goNext() {
        if (_submitting) return;

        if (_currentStep === 1) {
            // 校验字段
            if (!_validateModeFields(true)) return;

            // 检查测试状态
            if (!_testPassed) {
                var modeLabel = _currentMode === 'api' ? '线上 API' : '推理服务';
                RamariaModal.show({
                    title: '未测试连接',
                    body: '<p style="font-size:13px;color:var(--text-secondary);line-height:1.6;">' +
                          '您还没有测试' + modeLabel + '连接，可能无法正常对话。</p>' +
                          '<p style="font-size:12px;color:var(--text-tertiary);">建议先点击「测试连接」按钮确认配置正确。</p>',
                    footer: '<button class="btn btn-secondary" data-action="cancel">取消</button>' +
                            '<button class="btn btn-primary" data-action="skip">跳过测试，继续</button>',
                    onAction: function (action) {
                        if (action === 'skip') {
                            _showStep(2);
                        }
                    },
                });
                return;
            }

            _showStep(2);
        } else if (_currentStep === TOTAL_STEPS - 1) {
            // 确认 → 完成
            await _finishSetup();
        } else if (_currentStep === TOTAL_STEPS) {
            // 最终完成：刷新状态，Router 自动路由到对话页
            try {
                var newState = await RamariaApi.setup.refresh();
                RamariaStore.set('appState', newState);
                RamariaToast.show('success', '欢迎使用 Ramaria！');
            } catch (err) {
                RamariaStore.set('appState', 'ready');
                RamariaToast.show('success', '欢迎使用 Ramaria！');
            }
        }
    }

    // =========================================================
    // 完成配置
    // =========================================================

    async function _finishSetup() {
        _submitting = true;
        var btnNext = $('setup-btn-next');
        var btnPrev = $('setup-btn-prev');

        if (btnNext) btnNext.disabled = true;
        if (btnPrev) btnPrev.disabled = true;

        // 显示进度
        var initStatus = $('setup-init-status');
        if (initStatus) initStatus.classList.add('visible');

        var config = _collectConfig();

        try {
            // 保存配置 + 运行首次设置
            var result = await RamariaApi.setup.run(
                config.provider,
                config.modelId,
                config.baseUrl,
                config.apiKey
            );

            console.log('[SetupView] 配置完成: ' + result);

            // 线上 provider 记录隐私确认
            if (_currentMode === 'api') {
                try { await RamariaApi.chat.confirmPrivacy(true); } catch (_) { /* ignore */ }
            }

            // 更新进度：保存成功
            _updateInitLine('setup-init-save', 'ok', '配置已保存');

            // 显示完成行
            var doneLine = $('setup-init-done');
            if (doneLine) doneLine.style.display = 'flex';
            _updateInitLine('setup-init-done', 'ok', '配置已保存，正在进入对话界面…');

            _showStep(TOTAL_STEPS);

            // 延迟自动进入
            setTimeout(async function () {
                try {
                    var newState = await RamariaApi.setup.refresh();
                    RamariaStore.set('appState', newState || 'ready');
                } catch (_) {
                    RamariaStore.set('appState', 'ready');
                }
            }, 1500);

        } catch (err) {
            var rawMsg = err.message || String(err);
            console.error('[SetupView] 配置保存失败:', rawMsg);

            // 构建上下文相关的错误提示
            // 如果用户跳过了连接测试，失败很可能是连接问题
            var hint = '';
            if (!_testPassed) {
                hint = '\n\n💡 您跳过了连接测试，这可能是服务不可达导致的。'
                     + '\n建议返回第一步点击「测试连接」确认配置正确。';
            }

            _updateInitLine('setup-init-save', 'fail', '保存失败：' + rawMsg);

            RamariaToast.show('error', '配置失败', rawMsg + hint,
                { duration: hint ? 8000 : 4000 });

            if (btnNext) btnNext.disabled = false;
            if (btnPrev) btnPrev.disabled = false;

            _showStep(TOTAL_STEPS - 1);
        } finally {
            _submitting = false;
        }
    }

    function _updateInitLine(id, state, text) {
        var el = $(id);
        if (!el) return;
        var icon = state === 'ok' ? '✓' : state === 'fail' ? '✗' : '⏳';
        var cls = state === 'ok' ? 'setup-mark-ok' : state === 'fail' ? 'setup-mark-fail' : 'setup-mark-wait';
        el.innerHTML = '<span class="' + cls + '">' + icon + '</span> ' + text;
        el.style.display = 'flex';
    }

    // =========================================================
    // 摘要填充
    // =========================================================

    function _fillSummary() {
        var box = $('setup-summary-box');
        if (!box) return;

        var config = _collectConfig();
        var modeLabel = _currentMode === 'api' ? '☁ 线上 API' : '🖥 本地部署';

        var apiKeyDisplay = '';
        if (_currentMode === 'api' && config.apiKey) {
            apiKeyDisplay = config.apiKey.length > 8
                ? config.apiKey.slice(0, 8) + '…'
                : config.apiKey;
        }

        var lines = [
            '<div><span class="setup-summary-dim">对话模型模式：</span>' + modeLabel + '</div>',
            '<div><span class="setup-summary-dim">' + (_currentMode === 'api' ? 'API 地址' : '推理服务地址') + '：</span>' + (config.baseUrl || '-') + '</div>',
            '<div><span class="setup-summary-dim">模型名称：</span>' + (config.modelId || '-') + '</div>',
        ];
        if (_currentMode === 'api' && apiKeyDisplay) {
            lines.push('<div><span class="setup-summary-dim">API Key：</span>' + apiKeyDisplay + '</div>');
        }

        box.innerHTML = lines.join('');
    }

    // =========================================================
    // 生命周期
    // =========================================================

    function _registerHooks() {
        var unreg;

        unreg = RamariaRouter.registerHook('setup', 'enter', function () {
            console.log('[SetupView] enter');
            render();
        });
        _unregisterFns.push(unreg);

        unreg = RamariaRouter.registerHook('setup', 'leave', function () {
            console.log('[SetupView] leave');
            _currentStep = 1;
            _currentMode = 'local';
            _testPassed = false;
            _submitting = false;
        });
        _unregisterFns.push(unreg);
    }

    function init() {
        console.log('[SetupView] 初始化配置向导...');
        _registerHooks();
    }

    // =========================================================
    // 公开 API
    // =========================================================

    return {
        init: init,
        render: render,
        destroy: function () {
            for (var i = 0; i < _unregisterFns.length; i++) {
                try { _unregisterFns[i](); } catch (_) { /* ignore */ }
            }
            _unregisterFns = [];
            console.log('[SetupView] 已销毁');
        },
    };
})();

// 自动初始化
(function _autoInit() {
    if (typeof RamariaRouter === 'undefined') {
        setTimeout(_autoInit, 50);
        return;
    }
    RamariaSetupView.init();

    var currentView = RamariaRouter.getCurrentView();
    if (currentView === 'setup') {
        setTimeout(function () {
            if (RamariaRouter.getCurrentView() === 'setup') {
                RamariaRouter.showView('setup', { forceReenter: true });
            }
        }, 10);
    }
})();

Object.defineProperty(window, 'RamariaSetupView', {
    value: RamariaSetupView,
    writable: false,
    configurable: false,
});
