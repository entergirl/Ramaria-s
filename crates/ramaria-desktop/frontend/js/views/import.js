/**
 * js/views/import.js — 数据导入视图模块
 *
 * 职责:
 * - 三步向导：选择文件 → 预览报告 → 确认导入
 * - 支持 qq-chat-exporter v6.x JSON 格式的 QQ 聊天记录导入
 * - 快速导入（仅 L0）和深度导入（L0 + 后台管线）两种模式
 * - 文件选择通过 Tauri dialog 打开系统文件选择器
 * - : 双画像支持——分别为导出者和对方创建独立 persona
 * - 导入结果含统计摘要和解析报告详情
 *
 * 生命周期:
 * - enter: 渲染导入向导 DOM，绑定事件
 * - leave: 清理临时状态和事件绑定
 *
 * 依赖:
 * - RamariaApi (js/api.js) — 调用 import_qq_chat / detect_qq_format / analyze_qq_chat
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
    /** 导入侧过滤：self | other | both */
    var _importSide = 'both';

 /** Persona 名称（导出者，: 保留向后兼容） */
    var _personaName = '';

 /** : 导出者 persona UID（可选，留空自动生成） */
    var _selfPersonaUid = '';

 /** : 对方 persona 名称（可选，默认使用文件中解析的对方名称） */
    var _otherPersonaName = '';

 /** : 对方 persona UID（可选，留空自动生成） */
    var _otherPersonaUid = '';

 /** Session 切割间隔（分钟） */
    var _gapMinutes = 10;

 /** 解析报告数据 */
    var _reportData = null;

 /** 是否正在导入中（防止重复提交） */
    var _isImporting = false;

 /** 导入后深度处理进度跟踪 */
    var _importProgress = {
        phase: '',           // 'l1' | 'l2' | 'l3' | 'done'
        current: 0,
        total: 0,
        message: '',
        l1Success: null,     // done 阶段：L1 成功数
        l1Failed: null,      // done 阶段：L1 失败数
        l2Triggered: null,   // done 阶段：深度模式 L2 是否已触发
        l3Triggered: null,   // done 阶段：深度模式 L3 是否已触发
        // v1.5 I：后端阶段预计总量与 EMA 剩余秒数（None/undefined = 未知，前端线性兜底）
        l1Expected: null,
        l2Expected: null,
        l3Expected: null,
        etaSeconds: null,
    };

    /** 导入开始时间（Unix 毫秒），用于预估剩余时间 */
    var _importStartedAt = null;

 /** 导入完成后用于导航的 persona 信息 */
    var _importResultPersona = {
        selfUid: '',         // 导出者 persona UID
        selfName: '',        // 导出者 persona 名称
        otherUid: '',        // 对方 persona UID
        otherName: '',       // 对方 persona 名称
    };

 /** Tauri event unlisten 函数（import-progress 监听器） */
    var _importProgressUnlisten = null;

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
        _importProgress = { phase: '', current: 0, total: 0, message: '', l1Success: null, l1Failed: null, l2Triggered: null, l3Triggered: null, l1Expected: null, l2Expected: null, l3Expected: null, etaSeconds: null };
        _importResultPersona = { selfUid: '', selfName: '', otherUid: '', otherName: '' };
        _importStartedAt = null;  // 重置 ETA 计时

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
 // ═══ 不立即清理 import-progress 监听 ═══
 // 后端 L1/L2/L3 管线是异步执行的，done 事件可能在用户离开导入页后才到达。
 // 保持监听器存活直到 done 事件到达（或超时 5 分钟），
 // 确保 L1 失败警告能通过全局 Toast 送达用户。
 // _onDestroyImportProgressListener 在 done 事件处理或超时后自动调用。
        console.log('[ImportView] 离开导入视图（保留进度监听器等待 done 事件）');
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

 // ── Post-render: 更新动态元素（避免内联 style 违反 CSP）──
        if (_step === 'importing') {
            _updateImportProgressBar();
        }

        _bindEvents();
    }

 /**
 * 更新导入进度条的宽度和 ETA（通过 CSSOM，CSP-safe）。
 *
 * 说明:
 * - CSP `style-src 'self'` 阻止 HTML 中的 `style="..."` 属性，
 * 但允许 JavaScript 通过 element.style 操作 CSSOM。
 * - 此函数在 `_render` 设置 innerHTML 后调用，更新进度条填充宽度、百分比、会话计数和 ETA。
 * - 进度条高度 ≥ 8px、显示"第 N/M 个会话"、预估剩余时间。
 */
    function _updateImportProgressBar() {
        var prog = _importProgress;
        if (prog.total > 0) {
            var pct = Math.round((prog.current / prog.total) * 100);

 // ── 进度条填充宽度 ──
            var bar = document.getElementById('import-progress-fill-inline');
            if (bar) {
                bar.style.width = pct + '%';  // CSSOM 操作，不触发 CSP
            }

 // ── 百分比文本 ──
            var pctEl = document.getElementById('import-progress-pct-inline');
            if (pctEl) {
                pctEl.textContent = pct + '% (' + prog.current + '/' + prog.total + ')';
            }

 // ── 会话计数器更新 ──
            var sessionCur = document.getElementById('import-session-current');
            if (sessionCur) {
                sessionCur.textContent = prog.current;
            }

 // ── 预估剩余时间 (ETA) ──
            _updateEta(prog, pct);
        }
    }

    /**
     * 计算并显示预估剩余时间。
     *
     * - 优先使用后端分层 EMA 估算结果（payload.eta_seconds，已含各阶段
     *   L1/L2/L3 剩余量 × EMA 单次耗时求和）。
     * - 后端不可得（无样本/首次运行/旧后端）→ 回退既有线性 rate 估算：
     *   elapsed = now - _importStartedAt; rate = elapsed / current;
     *   eta_seconds = rate × (total - current)。
     *
     * 格式:
     * - < 60秒: "约 N 秒"
     * - < 3600秒: "约 N 分钟"
     * - ≥ 3600秒: "约 N 小时 M 分钟"
     */
    function _updateEta(prog, pct) {
        var etaEl = document.getElementById('import-progress-eta');
        if (!etaEl) return;

        if (!_importStartedAt || prog.current === 0 || pct >= 100) {
            if (pct >= 100) {
                etaEl.textContent = '即将完成';
            } else {
                etaEl.textContent = '计算中...';
            }
            return;
        }

        var etaSec = null;

 // ── 后端 EMA 优先（v1.5 I）──
        if (typeof prog.etaSeconds === 'number' && prog.etaSeconds >= 0) {
            etaSec = Math.round(prog.etaSeconds);
        } else {
 // ── 线性 rate 估算兜底（后端统计不可得时）──
            var now = Date.now();
            var elapsedSec = (now - _importStartedAt) / 1000;

 // 至少等待 2 秒再计算（避免初始波动导致荒谬的 ETA）
            if (elapsedSec < 2) {
                etaEl.textContent = '计算中...';
                return;
            }

            var ratePerItem = elapsedSec / prog.current;          // 每会话秒数
            var remaining = prog.total - prog.current;
            etaSec = Math.round(ratePerItem * remaining);
        }

 // 格式化为人类可读
        if (etaSec < 60) {
            etaEl.textContent = '约 ' + etaSec + ' 秒';
        } else if (etaSec < 3600) {
            var min = Math.round(etaSec / 60);
            etaEl.textContent = '约 ' + min + ' 分钟';
        } else {
            var hr = Math.floor(etaSec / 3600);
            var minRem = Math.round((etaSec % 3600) / 60);
            etaEl.textContent = '约 ' + hr + ' 小时 ' + minRem + ' 分钟';
        }
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
            html += '<div class="import-file-info-name">' + RamariaEscape.escapeHtml(_selectedFileName) + '</div>';
            html += '<div class="import-file-info-size">' + (_selectedFileSize || '未知大小') + '</div>';
            html += '</div>';
            html += '</div>';
            html += '<div class="mt-3">';
            html += '<button class="btn btn-ghost btn-sm" id="btn-change-file">重新选择</button>';
            html += '</div>';
            html += '</div>';
        } else {
            html += '<div class="import-file-zone" id="import-file-zone">';
            html += '<div class="import-file-zone-icon">📂</div>';
            html += '<div class="import-file-zone-title">选择 QQ 聊天记录文件</div>';
            html += '<div class="import-file-zone-desc">支持 shuakami/qq-chat-exporter v6.x 导出的 JSON 文件</div>';
            html += '<button class="btn btn-primary" id="btn-select-file">浏览文件</button>';
            html += '</div>';
        }

 // 配置选项
        html += '<div class="import-options mt-4">';

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

 // Persona 名称（导出者）
        html += '<div class="import-option-group">';
        html += '<div class="import-option-label">我的 Persona 名称（可选）</div>';
        html += '<div class="import-option-desc">导出的消息中，你自己的发言将关联到此 Persona。留空则使用文件中解析的导出者名称。</div>';
        html += '<input type="text" class="input" id="input-persona-name" placeholder="例如: 小王" value="' + RamariaEscape.escapeHtml(_personaName) + '" />';
        html += '</div>';

 // 导出者 Persona UID
        html += '<div class="import-option-group">';
        html += '<div class="import-option-label">我的 Persona UID（可选）</div>';
        html += '<div class="import-option-desc">指定 UID（如 char-123456789）。留空则根据 QQ 号自动生成。</div>';
        html += '<input type="text" class="input" id="input-self-persona-uid" placeholder="char-123456789" value="' + RamariaEscape.escapeHtml(_selfPersonaUid) + '" />';
        html += '</div>';

 // 对方 Persona 名称
        html += '<div class="import-option-group">';
        html += '<div class="import-option-label">对方 Persona 名称（可选）</div>';
        html += '<div class="import-option-desc">对话中对方的发言将关联到此 Persona。留空则使用文件中解析的对方名称。</div>';
        html += '<input type="text" class="input" id="input-other-persona-name" placeholder="例如: 好友小李" value="' + RamariaEscape.escapeHtml(_otherPersonaName) + '" />';
        html += '</div>';

 // 对方 Persona UID
        html += '<div class="import-option-group">';
        html += '<div class="import-option-label">对方 Persona UID（可选）</div>';
        html += '<div class="import-option-desc">指定 UID。留空则根据 QQ 号自动生成。</div>';
        html += '<input type="text" class="input" id="input-other-persona-uid" placeholder="char-123456789" value="' + RamariaEscape.escapeHtml(_otherPersonaUid) + '" />';
        html += '</div>';

 // 导入侧过滤（self/other/both）
        html += '<div class="import-option-group">';
        html += '<div class="import-option-label">导入侧（可选）</div>';
        html += '<div class="import-option-desc">只导入某一侧的消息：self=仅我的发言，other=仅对方发言，both=双方（默认）。跳过侧消息不入库、不创建对应 Persona。</div>';
        html += '<select class="input" id="input-import-side">';
        html += '<option value="both"' + (_importSide === 'both' ? ' selected' : '') + '>双方（默认）</option>';
        html += '<option value="self"' + (_importSide === 'self' ? ' selected' : '') + '>仅我方（self）</option>';
        html += '<option value="other"' + (_importSide === 'other' ? ' selected' : '') + '>仅对方（other）</option>';
        html += '</select>';
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
        html += '<div class="import-report-subtitle">' + RamariaEscape.escapeHtml(_selectedFileName) + '</div>';
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
        html += '<div class="import-stat-card info">';
        html += '<div class="import-stat-number info">' + (report.sessionCount || 0) + '</div>';
        html += '<div class="import-stat-label">📋 对话 Session</div>';
        html += '</div>';
        html += '</div>';

 // 详细信息（深色昵称 @浅色ID 格式，QQ 号也浅色）
        html += '<div class="import-report-details">';
        html += '<div class="import-report-section">';
        html += '<strong>导出者:</strong> ' + RamariaEscape.escapeHtml(report.selfName || '未知') + ' <span class="text-tertiary">@' + RamariaEscape.escapeHtml(report.selfId || '') + '</span>' + (report.selfUin ? ' <span class="text-tertiary">[QQ:' + RamariaEscape.escapeHtml(report.selfUin) + ']</span>' : '') + '<br />';
        if (report.otherName) {
            html += '<strong>对方:</strong> ' + RamariaEscape.escapeHtml(report.otherName) + ' <span class="text-tertiary">@' + RamariaEscape.escapeHtml(report.otherUid || '') + '</span>' + (report.otherUin ? ' <span class="text-tertiary">[QQ:' + RamariaEscape.escapeHtml(report.otherUin) + ']</span>' : '') + '<br />';
        } else {
            html += '<strong>对话对象:</strong> ' + RamariaEscape.escapeHtml(report.chatName || '未知') + '<br />';
        }
        html += '<strong>时间范围:</strong> ' + RamariaEscape.escapeHtml(report.timeRange || '未知') + '<br />';
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
        var prog = _importProgress;

        html += '<div class="import-progress">';
        html += '<div class="import-progress-spinner">';
        html += '<div class="spinner-ring spinner-ring--lg" aria-label="导入中"></div>';
        html += '</div>';

 // 阶段标题
        var titleText = '正在导入聊天记录...';
        if (prog.phase === 'l1') titleText = '正在生成 L1 会话摘要...';
        else if (prog.phase === 'l2') titleText = '正在提取 L2 事件...';
        else if (prog.phase === 'l3') titleText = '正在推断 L3 性格画像...';
        html += '<div class="import-progress-title">' + RamariaEscape.escapeHtml(titleText) + '</div>';

 // 进度描述
        var descText = '请耐心等待，处理大文件可能需要一些时间';
        if (prog.message) descText = prog.message;
        html += '<div class="import-progress-desc">' + RamariaEscape.escapeHtml(descText) + '</div>';

 // 进度条增强——放大高度 + 会话计数 + 预估剩余时间
        if (prog.total > 0) {
            html += '<div class="import-progress-bar-enhanced">';

 // ── 进度条（高度 ≥ 8px）──
            html += '<div class="progress-track import-progress-track">';
            html += '<div class="progress-fill progress-pink" id="import-progress-fill-inline"></div>';
            html += '</div>';

 // ── 进度信息行：百分比 + 会话计数 + ETA ──
            html += '<div class="import-progress-info">';
            html += '<span class="import-progress-pct" id="import-progress-pct-inline"></span>';

 // 阶段指示器——显示"已完成 x/y 项"（v1.5 I：L1 总量 = session × persona，以 LLM 调用项计）
            if (prog.phase === 'l1') {
                html += '<span class="import-progress-sep">·</span>';
                html += '<span class="import-progress-session-counter">已完成 <strong id="import-session-current">' + prog.current + '</strong> / ' + prog.total + ' 项</span>';
            }

 // 预估剩余时间
            html += '<span class="import-progress-sep">·</span>';
            html += '<span class="import-progress-eta" id="import-progress-eta">计算中...</span>';
            html += '</div>';

            html += '</div>';
        }

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
        var prog = _importProgress;

 // ═══ L1 失败警告条 ═══
 // 当导入完成但深度处理检测到 L1 摘要生成失败时，展示醒目的引导提示。
        var hasL1Warning = prog.l1Failed !== null && prog.l1Failed > 0;
        if (hasL1Warning) {
            html += '<div class="import-warning-banner">';
            html += '<div class="import-warning-icon">⚠️</div>';
            html += '<div class="import-warning-content">';
            html += '<div class="import-warning-title">L1 摘要生成失败 ' + prog.l1Failed + '/' + (prog.l1Success + prog.l1Failed) + ' 条</div>';
            html += '<div class="import-warning-desc">这通常意味着 LLM 模型暂不可用。请确认模型连接成功后，前往 <strong>记忆页面</strong> 选择对应人格，点击 <strong>「🔬 深度处理导入的消息」</strong> 重新生成。</div>';
            html += '</div>';
            html += '</div>';
        }

 // ═══ 快速模式提示条 ═══
        if (result.mode === 'fast') {
            html += '<div class="import-info-banner">';
            html += '<div class="import-info-icon">💡</div>';
            html += '<div class="import-info-content">';
            html += '<div class="import-info-desc">快速导入仅写入消息，未生成记忆摘要。如需深度分析，请前往 <strong>记忆页面</strong> 选择对应人格，点击 <strong>「🔬 深度处理导入的消息」</strong> 生成 L1/L2/L3 记忆。</div>';
            html += '</div>';
            html += '</div>';
        }

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
        html += '<div class="import-result-stat-value">' + RamariaEscape.escapeHtml(result.mode || '') + '</div>';
        html += '<div class="import-result-stat-label">导入模式</div>';
        html += '</div>';
 // 展示 L1 处理状态（如果 deep 模式或已有统计）
        if (prog.l1Success !== null) {
            html += '<div class="import-result-stat">';
            html += '<div class="import-result-stat-value">' + prog.l1Success + ' / ' + (prog.l1Success + prog.l1Failed) + '</div>';
            html += '<div class="import-result-stat-label">L1 生成成功</div>';
            html += '</div>';
        }
        html += '</div>';

        if (result.reportSummary) {
            html += '<div class="import-result-summary">' + RamariaEscape.escapeHtml(result.reportSummary) + '</div>';
        }

        html += '</div></div>';

 // ═══ 导航按钮组 ═══
 // 默认导向记忆页面（主操作），辅以"查看消息"和"再次导入"。
        html += '<div class="import-actions">';
        html += '<button class="btn btn-ghost" id="btn-new-import">导入另一个文件</button>';
        html += '<button class="btn btn-secondary" id="btn-goto-chat">查看导入消息</button>';
        html += '<button class="btn btn-primary" id="btn-goto-memory">前往记忆页面</button>';
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

 // Persona 名称输入（导出者）
        var inputPersona = $('input-persona-name');
        if (inputPersona) {
            inputPersona.addEventListener('input', function () {
                _personaName = inputPersona.value.trim();
            });
        }

 // 导出者 Persona UID 输入
        var inputSelfUid = $('input-self-persona-uid');
        if (inputSelfUid) {
            inputSelfUid.addEventListener('input', function () {
                _selfPersonaUid = inputSelfUid.value.trim();
            });
        }

 // 对方 Persona 名称输入
        var inputOtherName = $('input-other-persona-name');
        if (inputOtherName) {
            inputOtherName.addEventListener('input', function () {
                _otherPersonaName = inputOtherName.value.trim();
            });
        }

 // 对方 Persona UID 输入
        var inputOtherUid = $('input-other-persona-uid');
        if (inputOtherUid) {
            inputOtherUid.addEventListener('input', function () {
                _otherPersonaUid = inputOtherUid.value.trim();
            });
        }

 // 导入侧下拉（self/other/both）
        var inputSide = $('input-import-side');
        if (inputSide) {
            inputSide.addEventListener('change', function () {
                _importSide = inputSide.value;
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
                _importProgress = { phase: '', current: 0, total: 0, message: '', l1Success: null, l1Failed: null, l2Triggered: null, l3Triggered: null, l1Expected: null, l2Expected: null, l3Expected: null, etaSeconds: null };
                _importStartedAt = null;  // 重置 ETA 计时
                _render();
            });
        }

 // 前往记忆页面按钮（done 阶段主操作）
        var btnGotoMemory = $('btn-goto-memory');
        if (btnGotoMemory) {
            btnGotoMemory.addEventListener('click', _navigateToMemory);
            _cleanupFns.push(function () { btnGotoMemory.removeEventListener('click', _navigateToMemory); });
        }

 // 查看导入消息按钮（done 阶段辅助操作）
        var btnGotoChat = $('btn-goto-chat');
        if (btnGotoChat) {
            btnGotoChat.addEventListener('click', _navigateToChat);
            _cleanupFns.push(function () { btnGotoChat.removeEventListener('click', _navigateToChat); });
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
                            extensions: ['json'],
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
                            extensions: ['json'],
                        }],
                        multiple: false,
                    });
                } catch (invokeErr) {
                    console.warn('[ImportView] 调用文件对话框失败:', invokeErr);
                }
            }

            if (!selected) {
                return; // 用户取消
            }

 // Tauri dialog 返回路径字符串或路径数组
            var filePath = typeof selected === 'string' ? selected : (selected.path || (Array.isArray(selected) ? selected[0] : null));

            if (!filePath) {
                RamariaToast.show('error', '错误', '未获取到文件路径');
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
            RamariaToast.show('error', '文件选择失败', err.message || String(err));
        }
    }

 // =========================================================
 // 事件处理：分析文件
 // =========================================================

    async function _handleAnalyze() {
        if (!_selectedFilePath) {
            RamariaToast.show('warning', '提示', '请先选择文件');
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
                RamariaToast.show('error', '格式错误', '文件格式不是 QQ 聊天记录，请确认文件来源。');
                if (btn) { btn.disabled = false; btn.textContent = '分析文件'; }
                return;
            }

 // 先展示占位预览，然后异步调用 analyze_qq_chat 获取完整解析报告

 // 模拟报告数据（实际应由后端 analyze 命令返回）
            _reportData = {
                selfName: '（解析中...）',
                selfId: '',
                selfUin: null,
                chatName: '（解析中...）',
                chatType: '',
                otherName: '',
                otherUid: '',
                otherUin: null,
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
 // 完整映射所有字段（含双方标识信息）
                    _reportData = {
                        selfName: report.self_name || '未知',
                        selfId: report.self_id || '',
                        selfUin: report.self_uin || null,
                        chatName: report.chat_name || '未知',
                        chatType: report.chat_type || '',
                        otherName: report.other_name || '',
                        otherUid: report.other_uid || '',
                        otherUin: report.other_uin || null,
                        timeRange: report.time_range || '未知',
                        totalSuccess: report.total_success || 0,
                        totalDegraded: report.total_degraded || 0,
                        totalSkipped: report.total_skipped || 0,
                        sessionCount: report.session_count || 0,
                    };

                    if (report.total_success === 0 && report.total_degraded === 0) {
                        RamariaToast.show('warning', '提示', '文件中没有可导入的消息');
                    }

                    _render();
                }
            } catch (parseErr) {
                console.error('[ImportView] 解析预览失败:', parseErr);
                RamariaToast.show('warning', '解析失败', parseErr.message || String(parseErr));
 // 不阻塞流程：用户可以继续尝试导入
            }

        } catch (err) {
            console.error('[ImportView] 文件分析失败:', err);
            RamariaToast.show('error', '分析失败', err.message || String(err));
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
            RamariaToast.show('error', '错误', '文件路径丢失，请重新选择文件');
            return;
        }

        _isImporting = true;
        _step = 'importing';
        _importStartedAt = Date.now();  // 记录开始时间用于 ETA 计算
        _render();

        console.log('[ImportView] 开始导入: file=' + _selectedFilePath + ', mode=' + _importMode + ', gap=' + _gapMinutes);

 // ── 注册 import-progress 事件监听（深度处理进度）──
 // 在后端异步生成 L1/L2/L3 时实时更新进度条；done 时获取最终统计。
        _setupImportProgressListener();

        try {
            var result = await RamariaApi.import.importQQ(
                _selectedFilePath,
                _importMode,
                _personaName || undefined,
                _selfPersonaUid || undefined,
                _otherPersonaName || undefined,
                _otherPersonaUid || undefined,
                _gapMinutes,
                _importSide
            );

            console.log('[ImportView] 导入返回结果:', result);

            if (result && result.success) {
                _reportData = {
                    sessionsWritten: result.sessions_written || 0,
                    messagesWritten: result.messages_written || 0,
                    mode: result.mode || _importMode,
                    reportSummary: result.report_summary || '',
                };

 // 保存 persona 信息供导航使用
                _importResultPersona = {
                    selfUid: result.persona_uid || '',
                    selfName: result.persona_name || '',
                    otherUid: result.other_persona_uid || '',
                    otherName: result.other_persona_name || '',
                };

                _step = 'done';
                _render();
                RamariaToast.show('success', '导入完成', (result.messages_written || 0) + ' 条消息已写入');
            } else {
                throw new Error(result ? '导入返回失败状态（success=false）' : '导入结果为空（无返回数据）');
            }
        } catch (err) {
            _step = 'select';
            _render();
 // 打印完整错误详情到控制台，便于诊断
            console.error('[ImportView] === 导入失败详情 ===');
            console.error('[ImportView] 错误对象:', err);
            console.error('[ImportView] 错误消息:', err.message || String(err));
            if (err.stack) console.error('[ImportView] 调用栈:', err.stack);
            console.error('[ImportView] === 导入失败详情结束 ===');
            RamariaToast.show('error', '导入失败', err.message || String(err));
        } finally {
            _isImporting = false;
        }
    }

 // =========================================================
 // 导入进度事件监听
 // =========================================================

 /**
 * 注册 Tauri import-progress 事件监听。
 *
 * 后端在导入完成后异步执行 L1/L2/L3 管线时发射此事件。
 * 前端用于：导入中页面显示实时进度、完成页面展示处理状态警告。
 *
 * 事件负载结构:
 * - `phase`: "l1" | "l2" | "l3" | "done"
 * - `current` / `total`: 进度计数
 * - `message`: 阶段描述
 * - `l1_success` / `l1_failed`: done 阶段统计（可选）
 * - `l2_triggered` / `l3_triggered`: done 阶段标记（可选）
 * - `l1_expected` / `l2_expected` / `l3_expected`: 各阶段预计总量（v1.5 I，可选）
 * - `eta_seconds`: 后端 EMA 估算剩余秒数（v1.5 I，可选；缺失时前端线性兜底）
 */
    function _setupImportProgressListener() {
 // 先清理旧监听器
        _onDestroyImportProgressListener();

        if (window.__TAURI__ && window.__TAURI__.event) {
            window.__TAURI__.event.listen('import-progress', function (event) {
                var payload = event.payload || {};
                console.log('[ImportView] import-progress:', payload.phase, payload.message);

                _importProgress.phase = payload.phase || '';
                _importProgress.current = payload.current || 0;
                _importProgress.total = payload.total || 0;
                _importProgress.message = payload.message || '';

 // v1.5 I：阶段预计总量与后端 EMA 剩余秒数（缺失时前端线性兜底）
                if (payload.l1_expected !== undefined) _importProgress.l1Expected = payload.l1_expected;
                if (payload.l2_expected !== undefined) _importProgress.l2Expected = payload.l2_expected;
                if (payload.l3_expected !== undefined) _importProgress.l3Expected = payload.l3_expected;
                if (payload.eta_seconds !== undefined) _importProgress.etaSeconds = payload.eta_seconds;

 // done 阶段携带最终统计
                if (payload.phase === 'done') {
                    _importProgress.l1Success = (payload.l1_success !== undefined) ? payload.l1_success : null;
                    _importProgress.l1Failed = (payload.l1_failed !== undefined) ? payload.l1_failed : null;
                    _importProgress.l2Triggered = (payload.l2_triggered !== undefined) ? payload.l2_triggered : null;
                    _importProgress.l3Triggered = (payload.l3_triggered !== undefined) ? payload.l3_triggered : null;

 // ── 用户可能已离开导入视图 ──
 // 如果 done 事件到达时用户不在导入页，通过全局 Toast 通知 L1 失败。
                    if (_step !== 'importing' && _step !== 'done') {
                        _notifyImportDoneViaToast();
                    }
 // 清理监听器（done 是最后一个事件）
                    _onDestroyImportProgressListener();
                }

 // 仍处于导入中页面时实时刷新进度条
                if (_step === 'importing') {
                    _render();
                }
 // 如果已是完成页面（后端 done 事件晚于页面渲染），重新渲染以展示 L1 警告
                if (_step === 'done' && payload.phase === 'done') {
                    _render();
                }
            }).then(function (unlisten) {
                _importProgressUnlisten = unlisten;
            }).catch(function (err) {
                console.warn('[ImportView] 注册 import-progress 监听失败:', err);
            });

 // ── 安全超时: 5 分钟后强制清理监听器 ──
 // 防止 done 事件永不抵达（网络/进程异常）导致监听器泄漏。
            setTimeout(function () {
                if (_importProgressUnlisten) {
                    console.warn('[ImportView] import-progress 监听器超时（5min），强制清理');
                    _onDestroyImportProgressListener();
                }
            }, 300000);
        }
    }

 /**
 * 通过全局 Toast 通知导入深度处理结果（用于用户已离开导入页的场景）。
 *
 * 说明:
 * - 后端 L1/L2/L3 管线异步执行，done 事件可能在用户浏览其他页面时到达。
 * - 此函数将 L1 失败统计转换为 Toast 提示，确保用户不会遗漏关键信息。
 */
    function _notifyImportDoneViaToast() {
        var prog = _importProgress;
        var l1Success = (prog.l1Success !== null) ? prog.l1Success : 0;
        var l1Failed = (prog.l1Failed !== null) ? prog.l1Failed : 0;

        if (l1Failed > 0) {
            RamariaToast.show(
                'warning', '导入深度处理完成',
                'L1 摘要生成成功 ' + l1Success + '/' + (l1Success + l1Failed) +
                '，失败 ' + l1Failed + ' 条。请确认 LLM 连接后前往记忆页面重试。',
                { duration: 8000 }
            );
        } else if (l1Success > 0) {
            RamariaToast.show(
                'success', '导入深度处理完成',
                'L1 摘要全部生成成功 (' + l1Success + ' 条)。记忆页面可查看结果。',
                { duration: 5000 }
            );
        }
    }

 /**
 * 安全清理 import-progress 事件监听器。
 */
    function _onDestroyImportProgressListener() {
        if (_importProgressUnlisten) {
            try { _importProgressUnlisten(); } catch (_) { /* ignore */ }
            _importProgressUnlisten = null;
        }
    }

 // =========================================================
 // 导航函数
 // =========================================================

 /**
 * 导航到记忆页面，默认选中导入的导出者 persona。
 *
 * 说明:
 * - 导入完成后用户最自然的下一步是查看生成的自传记忆。
 * - 如果 `selfUid` 为空（极端情况），仍导航到记忆页，由用户手动选择 persona。
 */
    function _navigateToMemory() {
        console.log('[ImportView] 导航到记忆页面, persona=' + _importResultPersona.selfUid);
        if (_importResultPersona.selfUid && RamariaStore) {
 // 预设 persona 选择器（记忆页面的 _refreshPersonaSelector 会读取此值）
            RamariaStore.set('preselectPersonaUid', _importResultPersona.selfUid);
        }
        if (RamariaRouter) {
            RamariaRouter.showView('memory', { forceReenter: true });
        }
    }

 /**
 * 导航到聊天页面查看导入的消息。
 *
 * 说明:
 * - 导入的 session 是已关闭的历史 session，应在 session 列表中查看（只读），
 * 而非作为活跃聊天自动加载。
 * - 设置 `viewingImportedSession` 标志，聊天页据此展示"导入对话"上下文和页眉。
 */
    function _navigateToChat() {
        console.log('[ImportView] 导航到聊天页面查看导入消息');
        if (RamariaStore) {
 // 标记接下来的 session 查看来自导入，聊天页据此调整标题和展示
            RamariaStore.set('viewingImportedSession', true);
            if (_importResultPersona.selfName) {
                RamariaStore.set('viewingImportedName', _importResultPersona.selfName);
            }
        }
        if (RamariaRouter) {
            RamariaRouter.showView('chat', { forceReenter: true });
        }
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
