/**
 * js/views/settings/sections/basic/diagnostics.js — Ramaria 设置页「诊断与更新」目录项
 *
 * 职责:
 * - 调试模式开关（写库后需重启生效；重启后显示侧边栏调试页与高级设置页签）
 * - 检查更新（当前版本 / 可更新徽标 / Release 说明）
 * - 导出诊断信息（zip 打包，含收集警告提示）
 *
 * 设计特点:
 * - 检查更新结果与 Release 内容先转义再拼 HTML（防注入）
 * - 用户取消导出时静默忽略；失败路径恢复按钮态并提示
 *
 * 依赖:
 * - NS.$（shared.js）
 * - RamariaApi / RamariaToast / RamariaEscape
 */

(function (NS) {
    'use strict';

    var state = NS.state;

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
                    '<input type="checkbox" id="settings-debug-enabled"' + (state.debugEnabled ? ' checked' : '') + ' /> ' +
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
        var checkBtn = NS.$('settings-check-update');
        if (checkBtn) checkBtn.addEventListener('click', _handleCheckUpdate);

        var exportDiagBtn = NS.$('settings-export-diagnostics');
        if (exportDiagBtn) exportDiagBtn.addEventListener('click', _handleExportDiagnostics);

        var debugBox = NS.$('settings-debug-enabled');
        if (debugBox) debugBox.addEventListener('change', _handleDebugToggle);
    }

 /**
 * 处理"启用调试"开关改动（v2.0 M7）。
 *
 * 语义: 立即写库（settings.debug_enabled），但 UI 生效需重启——
 * 不即时改动侧边栏/高级设置页签，避免用户误以为马上生效。
 */
    async function _handleDebugToggle() {
        var debugBox = NS.$('settings-debug-enabled');
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
        var checkBtn = NS.$('settings-check-update');
        var badgeEl = NS.$('settings-update-badge');
        var detailEl = NS.$('settings-update-detail');
        var msgEl = NS.$('settings-update-message');

        if (checkBtn) {
            checkBtn.disabled = true;
            checkBtn.textContent = '检查中...';
        }

        try {
            var result = await RamariaApi.diagnostics.checkUpdate();

 // 更新版本显示
            var versionEl = NS.$('settings-current-version');
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
        var exportBtn = NS.$('settings-export-diagnostics');
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
 // 挂载面
 // =========================================================

    NS.parts.diagnostics = {
        /** 渲染诊断与更新目录项（调试开关 / 版本 / 检查更新 / 诊断导出） */
        render: _renderDiagnosticsSection,
    };
})(window.RamariaSettingsNS);
