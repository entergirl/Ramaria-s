/**
 * js/views/settings/sections/basic/data_dir.js — Ramaria 设置页「数据目录」目录项
 *
 * 职责:
 * - 数据目录路径查看与修改（数据库 / 索引 / 日志存储位置）
 *
 * 设计特点:
 * - 保存经 update_full_config 统一写入口双写；修改后需重启应用生效
 *
 * 依赖:
 * - NS.$ / NS.handleUpdateFullResult（shared.js）
 * - RamariaApi / RamariaToast
 */

(function (NS) {
    'use strict';

    var state = NS.state;

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

        var btn = NS.$('settings-save-data-dir');
        if (btn) btn.addEventListener('click', _handleSaveDataDir);
    }

    /**
     * 回显数据目录（paths.data_dir）。
     */
    function _fillDataDirForm(config) {
        if (!config || !config.paths) return;
        var input = NS.$('settings-data-dir');
        if (input && typeof config.paths.data_dir === 'string') {
            input.value = config.paths.data_dir;
        }
    }

    /**
     * 保存数据目录：完整配置更新（统一写入口双写）。
     */
    async function _handleSaveDataDir() {
        try {
            if (!state.fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }
            var input = NS.$('settings-data-dir');
            if (!input) return;

            var cfg = JSON.parse(JSON.stringify(state.fullConfig));
            cfg.paths.data_dir = input.value.trim();

            var result = await RamariaApi.config.updateFull(cfg);
            if (!NS.handleUpdateFullResult(result, '数据目录已保存（重启后生效）')) {
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

    NS.parts.dataDir = {
        /** 渲染数据目录目录项 */
        render: _renderDataDirSection,
        /** 回显数据目录路径 */
        fill: _fillDataDirForm,
    };
})(window.RamariaSettingsNS);
