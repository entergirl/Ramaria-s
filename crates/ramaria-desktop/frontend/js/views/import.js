/**
 * js/views/import.js — 数据导入视图模块
 *
 * 职责:
 * - 三步向导：选择文件 → 预览报告 → 确认导入
 * - 支持 QQ 聊天记录导入（JSON 和 .txt 格式）
 * - 快速导入（仅 L0）和深度导入（L0 + 后台管线）两种模式
 * - 文件选择通过 Tauri dialog 打开系统文件选择器
 * - 导入结果含统计摘要和解析报告详情
 *
 * 生命周期:
 * - enter: 渲染导入向导 DOM，绑定事件
 * - leave: 清理临时状态和事件绑定
 *
 * 依赖:
 * - RamariaApi (js/api.js) — 调用 import_qq_chat / detect_qq_format
 * - RamariaStore (js/store.js) — 读取应用状态
 * - RamariaRouter (js/router.js) — 视图标题管理
 * - RamariaToast (js/components/toast.js) — 成功/错误提示
 */

var ImportView = (function () {
    'use strict';

    // =========================================================
    // 内部状态
    // =========================================================

    /** 当前步骤: 'select' | 'preview' | 'importing' | 'done' */
    var _step = 'select';

    /** 选中的文件路径 */
    var _selectedFilePath = null;

    /** 选中的文件名 */
    var _selectedFileName = null;

    /** 文件大小（人类可读） */
    var _selectedFileSize = null;

    /** 导入模式: 'fast' | 'deep' */
    var _importMode = 'fast';

    /** Persona 名称 */
    var _personaName = '';

    /** Session 切割间隔（分钟） */
    var _gapMinutes = 10;

    /** 解析报告数据 */
    var _reportData = null;

    /** 是否正在导入中（防止重复提交） */
    var _isImporting = false;

    /** 清理函数列表 */
    var _cleanupFns = [];

    // =========================================================
    // DOM 查询
    // =========================================================

    function $(id) {
        return document.getElementById(id);
    }

    function _getViewContainer() {
        return document.querySelector('.view[data-view="import"]');
    }

    // =========================================================
    // 生命周期钩子注册
    // =========================================================

    /**
     * 初始化：注册 enter/leave 钩子。
     * 由 app.js 在导入此模块后调用。
     */
    function init() {
        if (RamariaRouter) {
            RamariaRouter.registerHook('import', 'enter', _onEnter);
            RamariaRouter.registerHook('import', 'leave', _onLeave);
        }
        console.log('[ImportView] 初始化完成');
    }

    // =========================================================
    // enter 钩子：渲染向导 UI
    // =========================================================

    function _onEnter() {
        var container = _getViewContainer();
        if (!container) {
            console.error('[ImportView] 未找到导入视图容器');
            return;
        }

        // 重置状态
        _step = 'select';
        _selectedFilePath = null;
        _selectedFileName = null;
        _selectedFileSize = null;
        _importMode = 'fast';
        _reportData = null;
        _isImporting = false;

        // 设置标题
        RamariaRouter.setContentTitle('数据导入');
        RamariaRouter.setContentActions('');

        // 渲染步骤引导 + 文件选择区
        _render();

        console.log('[ImportView] 进入导入视图');
    }

    // =========================================================
    // leave 钩子：清理
    // =========================================================

    function _onLeave() {
        _cleanup();
        console.log('[ImportView] 离开导入视图');
    }

    function _cleanup() {
        _cleanupFns.forEach(function (fn) {
            try { fn(); } catch (_) { /* ignore */ }
        });
        _cleanupFns = [];
    }

    // =========================================================
    // 渲染核心
    // =========================================================

    function _render() {
        var container = _getViewContainer();
        if (!container) return;

        var html = '';

        // 步骤引导
        html += _renderSteps();

        // 主内容区
        html += '<div class="import-container">';

        switch (_step) {
            case 'select':
                html += _renderFileSelect();
                break;
            case 'preview':
                html += _renderPreview();
                break;
            case 'importing':
                html += _renderImporting();
                break;
            case 'done':
                html += _renderDone();
                break;
        }

        html += '</div>';

        container.innerHTML = html;
        _bindEvents();
    }

    // =========================================================
    // 步骤引导条
    // =========================================================

    function _renderSteps() {
        var steps = [
            { id: 'select', label: '选择文件' },
            { id: 'preview', label: '预览报告' },
            { id: 'importing', label: '执行导入' },
        ];

        // done 状态下显示完成
        if (_step === 'done') {
            steps.push({ id: 'done', label: '完成' });
        }

        var html = '<div class="import-steps">';
        steps.forEach(function (s, i) {
            var cls = 'import-step';
            if (s.id === _step) {
                cls += ' active';
            } else if (_isStepDone(s.id)) {
                cls += ' done';
            }
            html += '<div class="' + cls + '">';
            html += '<span class="import-step-number">' + (i + 1) + '</span>';
            html += '<span>' + s.label + '</span>';
            html += '</div>';
        });
        html += '</div>';

        return html;
    }

    function _isStepDone(stepId) {
        if (stepId === 'select') {
            return _step === 'preview' || _step === 'importing' || _step === 'done';
        }
        if (stepId === 'preview') {
            return _step === 'importing' || _step === 'done';
        }
        if (stepId === 'importing') {
            return _step === 'done';
        }
        return false;
    }

    // =========================================================
    // Step 1: 文件选择
    // =========================================================

    function _renderFileSelect() {
        var html = '';

        // 文件选择区域
        if (_selectedFilePath) {
            html += '<div class="import-file-zone has-file" id="import-file-zone">';
            html += '<div class="import-file-zone-icon">📄</div>';
            html += '<div class="import-file-zone-title">已选择文件</div>';
            html += '<div class="import-file-info">';
            html += '<div class="import-file-info-icon">📁</div>';
            html += '<div class="import-file-info-details">';
            html += '<div class="import-file-info-name">' + _escapeHtml(_selectedFileName) + '</div>';
            html += '<div class="import-file-info-size">' + (_selectedFileSize || '未知大小') + '</div>';
            html += '</div>';
            html += '</div>';
            html += '<div style="margin-top:var(--space-3)">';
            html += '<button class="btn btn-ghost btn-sm" id="btn-change-file">重新选择</button>';
            html += '</div>';
            html += '</div>';
        } else {
            html += '<div class="import-file-zone" id="import-file-zone">';
            html += '<div class="import-file-zone-icon">📂</div>';
            html += '<div class="import-file-zone-title">选择 QQ 聊天记录文件</div>';
            html += '<div class="import-file-zone-desc">支持 shuakami/qq-chat-exporter JSON 或 PCQQ .txt 导出文件</div>';
            html += '<button class="btn btn-primary" id="btn-select-file">浏览文件</button>';
            html += '</div>';
        }

        // 配置选项
        html += '<div class="import-options" style="margin-top:var(--space-4)">';

        // 导入模式
        html += '<div class="import-option-group">';
        html += '<div class="import-option-label">导入模式</div>';
        html += '<div class="import-mode-selector">';
        html += '<div class="import-mode-card' + (_importMode === 'fast' ? ' selected' : '') + '" data-mode="fast">';
        html += '<div class="import-mode-card-title">⚡ 快速导入</div>';
        html += '<div class="import-mode-card-desc">仅写入对话记录（L0），不触发记忆管线。适合快速预览历史对话。</div>';
        html += '<div class="import-mode-card-tag fast">推荐</div>';
        html += '</div>';
        html += '<div class="import-mode-card' + (_importMode === 'deep' ? ' selected' : '') + '" data-mode="deep">';
        html += '<div class="import-mode-card-title">🔬 深度导入</div>';
        html += '<div class="import-mode-card-desc">写入 L0 后由后台线程触发 L1 摘要、L2 事件和 L3 性格画像生成。</div>';
        html += '<div class="import-mode-card-tag deep">全管线</div>';
        html += '</div>';
        html += '</div>';
        html += '</div>';

        // Persona 名称
        html += '<div class="import-option-group">';
        html += '<div class="import-option-label">Persona 名称（可选）</div>';
        html += '<div class="import-option-desc">导入的消息将关联到此 Persona。留空则使用文件中解析的导出者名称。</div>';
        html += '<input type="text" class="input" id="input-persona-name" placeholder="例如: 好友小王" value="' + _escapeHtml(_personaName) + '" />';
        html += '</div>';

        // 切割间隔
        html += '<div class="import-option-group">';
        html += '<div class="import-option-label">Session 切割间隔（分钟）</div>';
        html += '<div class="import-option-desc">相邻消息间隔超过此值则创建新对话 session。默认 10 分钟。</div>';
        html += '<input type="number" class="input" id="input-gap-minutes" min="1" max="1440" value="' + _gapMinutes + '" />';
        html += '</div>';

        html += '</div>';

        // 操作按钮
        html += '<div class="import-actions">';
        html += '<button class="btn btn-primary" id="btn-analyze" ' + (_selectedFilePath ? '' : 'disabled') + '>分析文件</button>';
        html += '</div>';

        return html;
    }

    // =========================================================
    // Step 2: 预览报告
    // =========================================================

    function _renderPreview() {
        if (!_reportData) {
            return '<div class="import-error"><div class="import-error-title">错误</div>报告数据丢失，请返回重新选择文件。</div>';
        }

        var report = _reportData;
        var html = '';

        // 报告卡片
        html += '<div class="import-report">';
        html += '<div class="import-report-header">';
        html += '<div class="import-report-title">📊 文件解析报告</div>';
        html += '<div class="import-report-subtitle">' + _escapeHtml(_selectedFileName) + '</div>';
        html += '</div>';
        html += '<div class="import-report-body">';

        // 统计卡片
        html += '<div class="import-stat-grid">';
        html += '<div class="import-stat-card success">';
        html += '<div class="import-stat-number success">' + (report.totalSuccess || 0) + '</div>';
        html += '<div class="import-stat-label">✅ 成功解析</div>';
        html += '</div>';
        html += '<div class="import-stat-card degraded">';
        html += '<div class="import-stat-number degraded">' + (report.totalDegraded || 0) + '</div>';
        html += '<div class="import-stat-label">⚠️ 降级处理</div>';
        html += '</div>';
        html += '<div class="import-stat-card skipped">';
        html += '<div class="import-stat-number skipped">' + (report.totalSkipped || 0) + '</div>';
        html += '<div class="import-stat-label">⏭️ 已跳过</div>';
        html += '</div>';
        html += '<div class="import-stat-card" style="background:var(--blue-50);border:1px solid var(--blue-200)">';
        html += '<div class="import-stat-number" style="color:var(--color-secondary)">' + (report.sessionCount || 0) + '</div>';
        html += '<div class="import-stat-label">📋 对话 Session</div>';
        html += '</div>';
        html += '</div>';

        // 详细信息
        html += '<div class="import-report-details">';
        html += '<div class="import-report-section">';
        html += '<strong>导出者:</strong> ' + _escapeHtml(report.selfName || '未知') + ' (' + _escapeHtml(report.selfId || '') + ')<br />';
        html += '<strong>对话对象:</strong> ' + _escapeHtml(report.chatName || '未知') + '<br />';
        html += '<strong>时间范围:</strong> ' + _escapeHtml(report.timeRange || '未知') + '<br />';
        html += '<strong>Session 切割:</strong> ' + report.sessionCount + ' 个会话（间隔 ' + _gapMinutes + ' 分钟）';
        html += '</div>';
        html += '</div>';

        html += '</div></div>';

        // 操作按钮
        html += '<div class="import-actions">';
        html += '<button class="btn btn-ghost" id="btn-back-select">← 重新选择</button>';
        html += '<button class="btn btn-primary" id="btn-start-import">确认导入</button>';
        html += '</div>';

        return html;
    }

    // =========================================================
    // Step 3: 导入中
    // =========================================================

    function _renderImporting() {
        var html = '';

        html += '<div class="import-progress">';
        html += '<div class="import-progress-spinner">';
        html += '<div class="spinner-ring spinner-ring--lg" aria-label="导入中"></div>';
        html += '</div>';
        html += '<div class="import-progress-title">正在导入聊天记录...</div>';
        html += '<div class="import-progress-desc">请耐心等待，处理大文件可能需要一些时间</div>';
        html += '<div class="import-progress-bar">';
        html += '<div class="progress-track tall">';
        html += '<div class="progress-fill progress-pink" style="width:60%"></div>';
        html += '</div>';
        html += '</div>';
        html += '</div>';

        return html;
    }

    // =========================================================
    // Step 4: 完成
    // =========================================================

    function _renderDone() {
        var result = _reportData; // 此时 reportData 已替换为导入结果
        if (!result) {
            return '<div class="import-error"><div class="import-error-title">错误</div>导入结果数据丢失。</div>';
        }

        var html = '';

        html += '<div class="import-result">';
        html += '<div class="import-result-header">';
        html += '<div class="import-result-icon">✅</div>';
        html += '<div class="import-result-title">导入完成</div>';
        html += '</div>';
        html += '<div class="import-result-body">';

        html += '<div class="import-result-stats">';
        html += '<div class="import-result-stat">';
        html += '<div class="import-result-stat-value">' + (result.sessionsWritten || 0) + '</div>';
        html += '<div class="import-result-stat-label">写入 Session</div>';
        html += '</div>';
        html += '<div class="import-result-stat">';
        html += '<div class="import-result-stat-value">' + (result.messagesWritten || 0) + '</div>';
        html += '<div class="import-result-stat-label">写入消息</div>';
        html += '</div>';
        html += '<div class="import-result-stat">';
        html += '<div class="import-result-stat-value">' + _escapeHtml(result.mode || '') + '</div>';
        html += '<div class="import-result-stat-label">导入模式</div>';
        html += '</div>';
        html += '</div>';

        if (result.reportSummary) {
            html += '<div class="import-result-summary">' + _escapeHtml(result.reportSummary) + '</div>';
        }

        html += '</div></div>';

        html += '<div class="import-actions">';
        html += '<button class="btn btn-primary" id="btn-new-import">导入另一个文件</button>';
        html += '</div>';

        return html;
    }

    // =========================================================
    // 事件绑定
    // =========================================================

    function _bindEvents() {
        _cleanup();

        // Step 1: 文件选择
        var fileZone = $('import-file-zone');
        var btnSelectFile = $('btn-select-file');
        var btnChangeFile = $('btn-change-file');

        if (btnSelectFile) {
            btnSelectFile.addEventListener('click', function (e) {
                e.stopPropagation(); // 阻止冒泡到 fileZone，避免触发两次
                _handleSelectFile();
            });
            _cleanupFns.push(function () { btnSelectFile.removeEventListener('click', _handleSelectFile); });
        }
        if (fileZone && !_selectedFilePath) {
            fileZone.addEventListener('click', _handleSelectFile);
            _cleanupFns.push(function () { fileZone.removeEventListener('click', _handleSelectFile); });
        }
        if (btnChangeFile) {
            btnChangeFile.addEventListener('click', function (e) {
                e.stopPropagation();
                _selectedFilePath = null;
                _selectedFileName = null;
                _selectedFileSize = null;
                _reportData = null;
                _render();
            });
            _cleanupFns.push(function () { btnChangeFile.removeEventListener('click', arguments[0]); });
        }

        // 模式选择卡片
        var modeCards = document.querySelectorAll('.import-mode-card');
        modeCards.forEach(function (card) {
            card.addEventListener('click', function () {
                _importMode = card.getAttribute('data-mode');
                _render();
            });
        });

        // Persona 名称输入
        var inputPersona = $('input-persona-name');
        if (inputPersona) {
            inputPersona.addEventListener('input', function () {
                _personaName = inputPersona.value.trim();
            });
        }

        // Gap 输入
        var inputGap = $('input-gap-minutes');
        if (inputGap) {
            inputGap.addEventListener('input', function () {
                var v = parseInt(inputGap.value, 10);
                _gapMinutes = isNaN(v) || v < 1 ? 10 : v;
            });
        }

        // 分析按钮
        var btnAnalyze = $('btn-analyze');
        if (btnAnalyze) {
            btnAnalyze.addEventListener('click', _handleAnalyze);
            _cleanupFns.push(function () { btnAnalyze.removeEventListener('click', _handleAnalyze); });
        }

        // 返回按钮
        var btnBack = $('btn-back-select');
        if (btnBack) {
            btnBack.addEventListener('click', function () {
                _step = 'select';
                _reportData = null;
                _render();
            });
        }

        // 确认导入按钮
        var btnStart = $('btn-start-import');
        if (btnStart) {
            btnStart.addEventListener('click', _handleStartImport);
            _cleanupFns.push(function () { btnStart.removeEventListener('click', _handleStartImport); });
        }

        // 重新导入按钮
        var btnNew = $('btn-new-import');
        if (btnNew) {
            btnNew.addEventListener('click', function () {
                _step = 'select';
                _selectedFilePath = null;
                _selectedFileName = null;
                _selectedFileSize = null;
                _reportData = null;
                _render();
            });
        }
    }

    // =========================================================
    // 事件处理：选择文件
    // =========================================================

    async function _handleSelectFile() {
        try {
            // 使用 Tauri dialog plugin 的原生打开文件对话框
            var selected = null;

            if (window.__TAURI__ && window.__TAURI__.dialog && window.__TAURI__.dialog.open) {
                try {
                    selected = await window.__TAURI__.dialog.open({
                        title: '选择 QQ 聊天记录文件',
                        filters: [{
                            name: 'QQ 聊天记录',
                            extensions: ['json', 'txt'],
                        }],
                        multiple: false,
                        directory: false,
                    });
                } catch (dialogErr) {
                    console.warn('[ImportView] 原生文件对话框失败:', dialogErr);
                }
            }

            // 回退：通过 TauriBridge 调用
            if (!selected && TauriBridge && TauriBridge.invoke) {
                try {
                    selected = await TauriBridge.invoke('plugin:dialog|open', {
                        title: '选择 QQ 聊天记录文件',
                        filters: [{
                            name: 'QQ 聊天记录',
                            extensions: ['json', 'txt'],
                        }],
                        multiple: false,
                    });
                } catch (invokeErr) {
                    console.warn('[ImportView] invoke 文件对话框失败:', invokeErr);
                }
            }

            if (!selected) {
                return; // 用户取消
            }

            // Tauri dialog 返回路径字符串或路径数组
            var filePath = typeof selected === 'string' ? selected : (selected.path || (Array.isArray(selected) ? selected[0] : null));

            if (!filePath) {
                RamariaToast.show('未获取到文件路径', 'error');
                return;
            }

            // 更新状态
            _selectedFilePath = filePath;
            _selectedFileName = filePath.split(/[/\\]/).pop();
            _reportData = null;
            _step = 'select';

            // 尝试获取文件大小（通过 Tauri fs 或直接显示未知）
            _selectedFileSize = '未知大小';

            _render();

            console.log('[ImportView] 已选择文件: ' + filePath);
        } catch (err) {
            console.error('[ImportView] 文件选择失败:', err);
            RamariaToast.show('文件选择失败: ' + (err.message || String(err)), 'error');
        }
    }

    // =========================================================
    // 事件处理：分析文件
    // =========================================================

    async function _handleAnalyze() {
        if (!_selectedFilePath) {
            RamariaToast.show('请先选择文件', 'warning');
            return;
        }

        var btn = $('btn-analyze');
        if (btn) {
            btn.disabled = true;
            btn.textContent = '正在分析...';
        }

        try {
            // 先检测格式
            var isQQ = await RamariaApi.import.detectFormat(_selectedFilePath);

            if (!isQQ) {
                RamariaToast.show('文件格式不是 QQ 聊天记录，请确认文件来源。', 'error');
                if (btn) { btn.disabled = false; btn.textContent = '分析文件'; }
                return;
            }

            // 解析文件获取报告（后端 parse 后返回报告但不写入）
            // 前端的"预览"模式：直接通过 import_qq_chat 的 dry-run 或单独命令
            // 这里我们调 detect_format 确认格式，然后用一次完整导入来做预览
            // 实际方案：添加一个 analyze_qq_chat 命令，或者在前端直接展示文件基本信息
            // 
            // 简化方案：在后端添加 analyze 模式，但这里我们用 import_qq_chat 
            // 传入 mode=fast 但先不执行。更实际的方案是通过单独的 Tauri 命令来完成解析。
            //
            // 对前端来说，"预览"步骤展示的是文件级别的元信息。
            // 完整解析报告需要在后端单独实现。当前先展示基本文件信息。

            // 获取文件元信息
            var fileInfo = {
                fileName: _selectedFileName,
                filePath: _selectedFilePath,
            };

            // 模拟报告数据（实际应由后端 analyze 命令返回）
            _reportData = {
                selfName: '（解析中...）',
                selfId: '',
                chatName: '（解析中...）',
                timeRange: '（解析中...）',
                totalSuccess: 0,
                totalDegraded: 0,
                totalSkipped: 0,
                sessionCount: 0,
            };

            _step = 'preview';
            _render();

            // 异步执行完整解析
            try {
                var report = await RamariaApi.import.analyzeFile(_selectedFilePath, _gapMinutes);

                if (report) {
                    _reportData = {
                        selfName: report.self_name || '未知',
                        selfId: report.self_id || '',
                        chatName: report.chat_name || '未知',
                        timeRange: report.time_range || '未知',
                        totalSuccess: report.total_success || 0,
                        totalDegraded: report.total_degraded || 0,
                        totalSkipped: report.total_skipped || 0,
                        sessionCount: report.session_count || 0,
                    };

                    if (report.total_success === 0 && report.total_degraded === 0) {
                        RamariaToast.show('文件中没有可导入的消息', 'warning');
                    }

                    _render();
                }
            } catch (parseErr) {
                console.error('[ImportView] 解析预览失败:', parseErr);
                RamariaToast.show('文件解析预览失败: ' + (parseErr.message || String(parseErr)), 'warning');
                // 不阻塞流程：用户可以继续尝试导入
            }

        } catch (err) {
            console.error('[ImportView] 文件分析失败:', err);
            RamariaToast.show('文件分析失败: ' + (err.message || String(err)), 'error');
            if (btn) { btn.disabled = false; btn.textContent = '分析文件'; }
        } finally {
            if (btn) { btn.disabled = false; btn.textContent = '分析文件'; }
        }
    }

    // =========================================================
    // 事件处理：开始导入
    // =========================================================

    async function _handleStartImport() {
        if (_isImporting) return;
        if (!_selectedFilePath) {
            RamariaToast.show('文件路径丢失，请重新选择文件', 'error');
            return;
        }

        _isImporting = true;
        _step = 'importing';
        _render();

        try {
            var result = await RamariaApi.import.importQQ(
                _selectedFilePath,
                _importMode,
                _personaName || undefined,
                _gapMinutes
            );

            if (result && result.success) {
                _reportData = {
                    sessionsWritten: result.sessions_written || 0,
                    messagesWritten: result.messages_written || 0,
                    mode: result.mode || _importMode,
                    reportSummary: result.report_summary || '',
                };
                _step = 'done';
                _render();
                RamariaToast.show('导入完成! ' + (result.messages_written || 0) + ' 条消息已写入', 'success');
            } else {
                throw new Error(result ? '导入返回失败状态' : '导入结果为空');
            }
        } catch (err) {
            _step = 'select';
            _render();
            console.error('[ImportView] 导入失败:', err);
            RamariaToast.show('导入失败: ' + (err.message || String(err)), 'error');
        } finally {
            _isImporting = false;
        }
    }

    // =========================================================
    // 辅助函数
    // =========================================================

    /**
     * HTML 实体转义，防止 XSS。
     */
    function _escapeHtml(text) {
        if (!text) return '';
        return String(text)
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;')
            .replace(/'/g, '&#039;');
    }

    // =========================================================
    // 公开 API
    // =========================================================

    return {
        init: init,
    };
})();

// 自动初始化
(function _autoInit() {
    if (typeof RamariaRouter === 'undefined') {
        setTimeout(_autoInit, 50);
        return;
    }
    ImportView.init();
})();

// 防止意外覆盖
Object.defineProperty(window, 'ImportView', {
    value: ImportView,
    writable: false,
    configurable: false,
});
