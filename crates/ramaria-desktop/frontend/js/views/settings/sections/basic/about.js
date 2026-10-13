/**
 * js/views/settings/sections/basic/about.js — Ramaria 设置页「关于」目录项
 *
 * 职责:
 * - 版本号 / 项目链接 / 许可证展示（版本号经 shared.js 的 loadVersion 回填）
 *
 * 设计特点:
 * - 纯静态内容渲染；版本回显元素 id 保持不变（settings-about-version）
 *
 * 依赖:
 * - 无（NS 由 shared.js 创建）
 */

(function (NS) {
    'use strict';

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
            '<div class="settings-about-version" id="settings-about-version">加载中...</div>' +
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
 // 挂载面
 // =========================================================

    NS.parts.about = {
        /** 渲染关于目录项 */
        render: _renderAboutSection,
    };
})(window.RamariaSettingsNS);
