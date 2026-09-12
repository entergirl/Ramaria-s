/**
 * js/utils/bubble.js — 回复多气泡切分（纯函数）
 *
 * 职责:
 * - 把助手回复文本按分隔符 `||` 切分为多条气泡文本（前后端契约）。
 * - 只做切分与清洗（trim、丢弃空段），不渲染、不触碰 DOM。
 *
 * 设计特点:
 * - 分隔符契约：单条回复内的多条短句用 `||` 分隔；正文自身的换行**不**参与分隔。
 * - 无分隔符时返回单段；空内容返回空数组（调用方自行决定占位）。
 *
 * 用法:
 * var segments = RamariaBubble.splitBubbles('先去吃饭||饿着扛可不行');
 *
 * 依赖: 无。
 */

var RamariaBubble = (function () {
    'use strict';

// =========================================================
// 常量
// =========================================================

    /** 多气泡分隔符（前后端契约，与提示词「核心规则」一致，勿随意变更） */
    var SEPARATOR = '||';

// =========================================================
// 切分函数
// =========================================================

    /**
 * 将回复文本切分为多条气泡文本。
 *
 * 参数:
 * - `content`: 回复原文（可能含分隔符）。
 *
 * 返回:
 * - 非空段数组（每段已 trim）；无分隔符时为单段；非字符串/空串为 []。
 */
    function splitBubbles(content) {
        if (typeof content !== 'string' || content === '') return [];

        var parts = content.split(SEPARATOR);
        var out = [];
        for (var i = 0; i < parts.length; i++) {
            var seg = parts[i].trim();
            if (seg !== '') out.push(seg);
        }
        return out;
    }

// =========================================================
// 公开 API
// =========================================================

    return {
        SEPARATOR: SEPARATOR,
        splitBubbles: splitBubbles,
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaBubble', {
    value: RamariaBubble,
    writable: false,
    configurable: false,
});
