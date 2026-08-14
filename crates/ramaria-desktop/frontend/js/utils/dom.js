/**
 * js/utils/dom.js — Ramaria DOM/HTML 工具
 *
 * 职责:
 * - HTML 转义：先转义再拼 innerHTML，防止 XSS
 *
 * 设计特点:
 * - 通过 RamariaEscape 全局单例访问，纯函数无状态
 * - 同时转义 `&` `<` `>` `"` `'`，覆盖属性值语境（href/value/data-* 等）与文本语境
 * - null/undefined 安全回退为空字符串
 * - 零外部依赖，不依赖 DOM（可在非浏览器环境安全调用）
 *
 * 用法:
 * RamariaEscape.escapeHtml('<img src=x onerror=alert(1)>'); // "&lt;img src=x onerror=alert(1)&gt;"
 *
 * 依赖: 无
 */

var RamariaEscape = (function () {
    'use strict';

    /**
     * HTML 转义（防 XSS）。
     *
     * 参数:
     * - `str`: 任意值，null/undefined 返回空字符串
     *
     * 返回:
     * - 转义后的字符串
     */
    function escapeHtml(str) {
        if (str == null) return '';
        return String(str)
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;')
            .replace(/'/g, '&#39;');
    }

    return {
        escapeHtml: escapeHtml
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaEscape', {
    value: RamariaEscape,
    writable: false,
    configurable: false,
});
