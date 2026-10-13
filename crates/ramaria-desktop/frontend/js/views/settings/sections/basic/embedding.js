/**
 * js/views/settings/sections/basic/embedding.js — Ramaria 设置页「嵌入模型」目录项
 *
 * 职责:
 * - 本地嵌入模型路径配置（校验 / 保存 / 降级提示）
 *
 * 设计特点:
 * - 校验与保存均经 setup 命令面；保存后刷新应用状态
 * - 模型就绪时展示维度与可用徽标，路径无效时提示降级模式
 *
 * 依赖:
 * - NS.$（shared.js）
 * - RamariaApi / RamariaToast / RamariaStore
 */

(function (NS) {
    'use strict';

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
        var validateBtn = NS.$('settings-validate-embedding');
        var saveBtn = NS.$('settings-save-embedding');
        if (validateBtn) validateBtn.addEventListener('click', _handleValidateEmbedding);
        if (saveBtn) saveBtn.addEventListener('click', _handleSaveEmbedding);
    }

 /**
 * 填充嵌入模型配置表单。
 */
    function _fillEmbeddingForm(config) {
        var pathEl = NS.$('settings-embedding-path');
        var statusGroup = NS.$('settings-embedding-status-group');
        var statusEl = NS.$('settings-embedding-status');
        var badgeEl = NS.$('settings-embedding-valid-badge');

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
        var pathEl = NS.$('settings-embedding-path');
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

        var validateBtn = NS.$('settings-validate-embedding');
        if (validateBtn) {
            validateBtn.disabled = true;
            validateBtn.textContent = '校验中...';
        }

        try {
            var result = await RamariaApi.setup.validateEmbeddingModel(path);
            var statusGroup = NS.$('settings-embedding-status-group');
            var statusEl = NS.$('settings-embedding-status');
            var badgeEl = NS.$('settings-embedding-valid-badge');

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
        var pathEl = NS.$('settings-embedding-path');
        var path = pathEl ? pathEl.value.trim() : '';

        var saveBtn = NS.$('settings-save-embedding');
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
 // 挂载面
 // =========================================================

    NS.parts.embedding = {
        /** 渲染嵌入模型目录项 */
        render: _renderEmbeddingSection,
        /** 回显嵌入模型配置（路径 + 就绪状态徽标） */
        fill: _fillEmbeddingForm,
    };
})(window.RamariaSettingsNS);
