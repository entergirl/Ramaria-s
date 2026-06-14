/**
 * js/theme-init.js — Ramaria 主题初始化
 *
 * 职责:
 * - 在任何 CSS 加载前设置 data-theme 属性，消除主题闪烁（FOUC）。
 * - 始终以浅色模式启动，深色模式不持久化，每次启动默认浅色。
 * - 从 index.html 内联脚本外部化，以支持严格的 CSP（无 'unsafe-inline'）。
 *
 * 加载顺序要求:
 * - 必须在 index.html 中作为第一个 <script> 加载（head 中位于所有 CSS 之前）。
 * - 不可使用 defer/async，否则 CSS 可能已解析渲染。
 */
(function () {
    'use strict';
    document.documentElement.setAttribute('data-theme', 'light');
})();
