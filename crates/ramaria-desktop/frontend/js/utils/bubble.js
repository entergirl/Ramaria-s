/**
 * js/utils/bubble.js — 回复多气泡切分（纯函数）
 *
 * 职责:
 * - 把助手回复文本按分隔符 `||` 切分为多条气泡文本（前后端契约）。
 * - 只做切分与清洗（trim、丢弃空段），不渲染、不触碰 DOM。
 *
 * 设计特点:
 * - 分隔符契约：单条回复内的多条短句用 `||` 分隔；正文自身的换行**不**参与分隔。
 * - 豁免区（CR2-COR-011）：围栏代码块（``` / ~~~）、行内代码（`…`）、
 *   Markdown 链接目标与裸 URL 内部的 `||` 属于正文，不参与切分，
 *   避免把代码/网址截断成两条气泡。
 * - 空结果语义：内容为空或全为分隔符/空白时返回空数组，
 *   由调用方回退“原文单泡”（不产生空气泡、不静默丢内容）。
 * - 流式展示：streamDisplay() 复用同一扫描器，把豁免区外的 `||` 临时替换为换行；
 *   finalize 时再按 splitBubbles 重排为多气泡（与历史回读同一口径）。
 *
 * 用法:
 * var segments = RamariaBubble.splitBubbles('先去吃饭||饿着扛可不行');
 * var preview  = RamariaBubble.streamDisplay('先去吃饭||饿着扛可不行'); // '先去吃饭\n饿着扛可不行'
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

    /** 裸 URL 判定前缀（命中后整段吞掉，内部 `||` 不切分） */
    var URL_PREFIX = /^(https?:\/\/|www\.|mailto:)/i;

    /**
 * 裸 URL 终止字符（空白、引号、右括号与中文标点）。
 *
 * 注意: 不把 ASCII `:` / `;` / `?` / `!` 列为终止符——它们是 URL 的合法字符
 * （scheme 冒号、端口、query 串），列入会把 URL 截断。
 */
    var URL_TERMINATOR = /[\s<>"'`)\]）】}，。、！？；：]/;

// =========================================================
// 内部辅助
// =========================================================

    /**
 * 统计 `content` 中从下标 `i` 开始、由同一字符 `ch` 组成的连续长度。
 *
 * 说明: 用于识别围栏代码块（3+ 个 ` 或 ~）与行内代码标记长度。
 */
    function _countRun(content, i, ch) {
        var j = i;
        while (j < content.length && content.charAt(j) === ch) j++;
        return j - i;
    }

    /** 生成 `count` 个 `ch` 组成的字符串（ES5 兼容，不依赖 String.repeat） */
    function _repeat(ch, count) {
        var s = '';
        for (var k = 0; k < count; k++) s += ch;
        return s;
    }

    /** 判断下标 `i` 处是否为裸 URL 起点（http/https、www.、mailto:） */
    function _isUrlStart(content, i) {
        return URL_PREFIX.test(content.substr(i, 16));
    }

    /**
 * 按 `||` 切分原文，返回**未清洗**的原始段数组（保留段两侧空白）。
 *
 * 单遍扫描状态机（不回溯，O(n)）:
 * - `fence`: 围栏代码块标记（``` / ~~~）。块内除“行首同字符、长度不小于开启标记”的
 *   闭合标记外，其余字符（含 `||`）全部按正文处理。
 * - `inline`: 行内代码标记（` / ``）。同长度的反引号串闭合，块内 `||` 不切分。
 * - `atLineStart`: 是否位于行首（容忍最多 3 个前导空格），用于识别围栏开闭。
 * - 链接目标 `](…)`：整段吞掉（含嵌套括号），内部 `||` 不切分。
 * - 裸 URL：从 scheme 起吞到空白/标点终止符，内部 `||` 不切分。
 *
 * 参数:
 * - `content`: 待扫描的原文（调用方已保证是字符串）。
 *
 * 返回:
 * - 原始段数组（至少 1 段；分隔符本身不保留）。
 *
 * 说明:
 * - Markdown 表格（`| a | b |`）不在渲染器支持范围内（见 markdown.js 说明），
 *   因此不为其单独设豁免；如需支持表格渲染，需在此同步扩展豁免区。
 */
    function _splitRaw(content) {
        var segs = [];
        var buf = '';
        var i = 0;
        var n = content.length;
        var fence = '';       // '' = 不在围栏代码块内；否则为开启标记（如 '```'）
        var inline = '';      // '' = 不在行内代码内；否则为开启标记（如 '`'）
        var atLineStart = true;

        while (i < n) {
            var ch = content.charAt(i);

// ── 1. 围栏代码块内部：只识别“行首闭合标记”，其余字符原样复制 ──
            if (fence !== '') {
                if (atLineStart && ch === fence.charAt(0)) {
                    var closeRun = _countRun(content, i, ch);
                    if (closeRun >= fence.length) {
                        fence = '';
                        buf += content.substr(i, closeRun);
                        i += closeRun;
                        atLineStart = false;
                        continue;
                    }
                }
                buf += ch;
                atLineStart = (ch === '\n');
                i++;
                continue;
            }

// ── 2. 围栏代码块开启：行首 3+ 个同字符（` 或 ~）──
            if (atLineStart && (ch === '`' || ch === '~')) {
                var openRun = _countRun(content, i, ch);
                if (openRun >= 3) {
                    fence = _repeat(ch, openRun);
                    buf += content.substr(i, openRun);
                    i += openRun;
                    atLineStart = false;
                    continue;
                }
            }

// ── 3. 行内代码：同长度反引号串开启/闭合，块内 `||` 不切分 ──
            if (ch === '`') {
                var tickRun = _countRun(content, i, '`');
                if (inline === '') {
                    inline = _repeat('`', tickRun);
                } else if (tickRun === inline.length) {
                    inline = '';
                }
                buf += content.substr(i, tickRun);
                i += tickRun;
                atLineStart = false;
                continue;
            }
            if (inline !== '') {
                buf += ch;
                atLineStart = (ch === '\n');
                i++;
                continue;
            }

// ── 4. Markdown 链接目标 `](…)`：整段吞掉（含嵌套括号）──
            if (ch === ']' && content.charAt(i + 1) === '(') {
                buf += '](';
                i += 2;
                var depth = 1;
                while (i < n && depth > 0) {
                    var inner = content.charAt(i);
                    if (inner === '(') depth++;
                    else if (inner === ')') depth--;
                    buf += inner;
                    i++;
                }
                atLineStart = false;
                continue;
            }

// ── 5. 裸 URL：整段吞掉（内部 `||` 不切分）──
            if (_isUrlStart(content, i)) {
                while (i < n && !URL_TERMINATOR.test(content.charAt(i))) {
                    buf += content.charAt(i);
                    i++;
                }
                atLineStart = false;
                continue;
            }

// ── 6. 分隔符：仅在豁免区之外切分 ──
            if (ch === '|' && content.charAt(i + 1) === '|') {
                segs.push(buf);
                buf = '';
                i += 2;
                atLineStart = false;
                continue;
            }

// ── 7. 普通字符 ──
            buf += ch;
            if (ch === '\n') {
                atLineStart = true;
            } else if (!(atLineStart && (ch === ' ' || ch === '\t'))) {
                atLineStart = false;
            }
            i++;
        }

        segs.push(buf);
        return segs;
    }

// =========================================================
// 对外函数
// =========================================================

    /**
 * 将回复文本切分为多条气泡文本。
 *
 * 参数:
 * - `content`: 回复原文（可能含分隔符）。
 *
 * 返回:
 * - 非空段数组（每段已 trim、豁免区内容不被切断）；
 *   无分隔符时为单段；非字符串/空串/全分隔符/全空白为 []（调用方回退原文单泡）。
 */
    function splitBubbles(content) {
        if (typeof content !== 'string' || content === '') return [];

        var raw = _splitRaw(content);
        var out = [];
        for (var i = 0; i < raw.length; i++) {
            var seg = raw[i].trim();
            if (seg !== '') out.push(seg);
        }
        return out;
    }

    /**
 * 流式展示用：把豁免区之外的 `||` 替换为换行（不 trim，避免逐字渲染抖动）。
 *
 * 参数:
 * - `content`: 已累积的回复文本。
 *
 * 返回:
 * - 展示文本；非字符串/空串返回 ''。
 *
 * 说明:
 * - 与 splitBubbles 共用同一扫描器，代码块/行内代码/URL 内的 `||` 保持原样。
 */
    function streamDisplay(content) {
        if (typeof content !== 'string' || content === '') return '';
        return _splitRaw(content).join('\n');
    }

// =========================================================
// 公开 API
// =========================================================

    return {
        SEPARATOR: SEPARATOR,
        splitBubbles: splitBubbles,
        streamDisplay: streamDisplay,
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaBubble', {
    value: RamariaBubble,
    writable: false,
    configurable: false,
});
