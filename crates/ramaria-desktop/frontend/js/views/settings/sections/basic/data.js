/**
 * js/views/settings/sections/basic/data.js — Ramaria 设置页「数据管理」目录项
 *
 * 职责:
 * - 记忆数据导出（JSON / Markdown）与检索索引重建
 *
 * 设计特点:
 * - 导出经 Tauri 原生保存对话框选择路径（见 shared.js 的 pickSavePath）
 * - 重建索引带确认弹窗，重建期间无法对话；失败路径提示不阻塞
 *
 * 依赖:
 * - NS.$ / NS.pickSavePath（shared.js）
 * - RamariaApi / RamariaToast / RamariaModal / RamariaStore
 */

(function (NS) {
    'use strict';

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
        var exportJsonBtn = NS.$('settings-export-json');
        if (exportJsonBtn) {
            exportJsonBtn.addEventListener('click', _handleExportJson);
        }

 // 导出 Markdown
        var exportMdBtn = NS.$('settings-export-md');
        if (exportMdBtn) {
            exportMdBtn.addEventListener('click', _handleExportMarkdown);
        }

 // 重建索引
        var rebuildBtn = NS.$('settings-rebuild-index');
        if (rebuildBtn) {
            rebuildBtn.addEventListener('click', _handleRebuildIndex);
        }
    }

    async function _handleExportJson() {
        try {
            var path = await NS.pickSavePath('ramaria-export.json', 'JSON 文件');
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
            var path = await NS.pickSavePath('ramaria-export.md', 'Markdown 文件');
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
 // 挂载面
 // =========================================================

    NS.parts.data = {
        /** 渲染数据管理目录项（导出 / 重建索引） */
        render: _renderDataSection,
    };
})(window.RamariaSettingsNS);
