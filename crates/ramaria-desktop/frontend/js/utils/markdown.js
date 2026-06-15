/**
 * js/utils/markdown.js — Ramaria 轻量 Markdown 渲染 + XSS 防护
 *
 * 职责:
 * - 将 LLM 输出的 Markdown 文本渲染为安全 HTML
 * - 支持：标题、粗体、斜体、代码块、行内代码、无序列表、有序列表、
 *   链接、段落、水平线、块引用
 * - 严格执行 XSS sanitize：移除所有 <script>/<iframe>/<object>/事件属性
 * - CSP 安全：不使用 eval() / new Function() / innerHTML 直接注入
 *
 * 设计特点:
 * - 通过 RamariaMarkdown 全局单例访问
 * - 三步处理：预处理（保护代码块）→ 逐行解析 → HTML 安全清理
 * - 代码块保护机制：先用占位符替换代码块，避免内部 Markdown 被误解析
 * - 输出仅包含安全标签：h1-h3, p, strong, em, code, pre, ul, ol, li, a, hr, blockquote
 * - 链接自动添加 target="_blank" rel="noopener noreferrer"
 * - 完全离线，零外部依赖
 *
 * 用法:
 *   var html = RamariaMarkdown.render('**粗体** 和 *斜体*');
 *   var safeHtml = RamariaMarkdown.sanitize(userProvidedHtml);
 *
 * 依赖: 无
 */

var RamariaMarkdown = (function () {
    'use strict';

    // =========================================================
    // 允许的 HTML 标签和属性白名单
    // =========================================================

    /** 允许的标签集合 */
    var ALLOWED_TAGS = {
        'h1': true, 'h2': true, 'h3': true, 'h4': true, 'h5': true, 'h6': true,
        'p': true,
        'strong': true, 'b': true,
        'em': true, 'i': true,
        'code': true, 'pre': true,
        'ul': true, 'ol': true, 'li': true,
        'a': true,
        'hr': true,
        'blockquote': true,
        'br': true,
        'span': true,
        'div': true
    };

    /** 允许的属性（按标签） */
    var ALLOWED_ATTRS = {
        'a': ['href', 'title', 'target', 'rel'],
        'span': ['class'],
        'div': ['class'],
        'pre': ['class'],
        'code': ['class']
    };

    /** 禁止的 URL 协议（防止 javascript: 等危险协议） */
    var FORBIDDEN_PROTOCOLS = /^(javascript|data|vbscript|file):/i;

    // =========================================================
    // 常量和正则
    // =========================================================

    /** 代码块占位符前缀 */
    var CODE_PLACEHOLDER_PREFIX = '\x00MDCB';

    /** 行内代码占位符前缀 */
    var INLINE_CODE_PREFIX = '\x00MDIC';

    // =========================================================
    // HTML Sanitizer（白名单过滤）
    // =========================================================

    /**
     * 清理 HTML 字符串，仅保留白名单标签和属性。
     *
     * 参数:
     * - `html`: 原始 HTML 字符串
     *
     * 返回:
     * - 清理后的安全 HTML 字符串
     *
     * 说明:
     * - 使用正则匹配所有标签，逐个检查是否在白名单中
     * - 移除所有事件处理器属性（onclick 等）
     * - 检查链接的 href 协议，禁止 javascript:/data: 等
     * - 不依赖 DOMParser（某些受限环境可能不可用）
     */
    function sanitize(html) {
        if (!html || typeof html !== 'string') return '';

        // 移除注释
        html = html.replace(/<!--[\s\S]*?-->/g, '');

        // 匹配所有标签：<tagname attr="val"> 或 </tagname> 或自闭合
        // 注意：<\/? 匹配 < 或 </，需通过 isClosing 判断保留闭合标签语义
        html = html.replace(/<\/?([a-zA-Z][a-zA-Z0-9]*)(\s[^>]*)?(\/)?>/g, function (match, tagName, attrs, selfClose) {
            tagName = tagName.toLowerCase();

            // 不在白名单中 → 转义为文本
            if (!ALLOWED_TAGS[tagName]) {
                return _escHtml(match);
            }

            // 闭合标签：没有属性，直接返回 </tagname>
            if (match.charAt(1) === '/') {
                return '</' + tagName + '>';
            }

            // 处理属性
            var cleanAttrs = '';
            if (attrs) {
                cleanAttrs = _cleanAttributes(tagName, attrs);
            }

            if (selfClose || tagName === 'br' || tagName === 'hr') {
                return '<' + tagName + cleanAttrs + ' />';
            }

            return '<' + tagName + cleanAttrs + '>';
        });

        return html;
    }

    /**
     * 清理标签属性。
     *
     * 参数:
     * - `tagName`: 标签名（小写）
     * - `attrStr`: 属性字符串（不含尖括号）
     *
     * 返回:
     * - 清理后的属性字符串（以空格开头），如果全部属性被移除则返回空串
     */
    function _cleanAttributes(tagName, attrStr) {
        var allowedForTag = ALLOWED_ATTRS[tagName] || [];
        var result = '';

        // 逐属性解析
        var attrRegex = /([a-zA-Z][a-zA-Z0-9-]*)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?/g;
        var match;

        while ((match = attrRegex.exec(attrStr)) !== null) {
            var attrName = match[1].toLowerCase();

            // 跳过事件处理器
            if (/^on/i.test(attrName)) continue;

            // 跳过不在白名单中的属性
            if (allowedForTag.indexOf(attrName) === -1) continue;

            var attrValue = match[2] || match[3] || match[4] || '';

            // 对 href 做协议检查
            if (attrName === 'href') {
                if (FORBIDDEN_PROTOCOLS.test(attrValue.trim())) {
                    attrValue = '#blocked';
                }
            }

            // 重新编码属性值
            result += ' ' + attrName + '="' + _escAttr(attrValue) + '"';
        }

        return result;
    }

    /**
     * HTML 实体转义（用于文本内容）。
     */
    function _escHtml(str) {
        return str
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;');
    }

    /**
     * 属性值转义（比 _escHtml 更严格，不能包含未转义的双引号）。
     */
    function _escAttr(str) {
        return str
            .replace(/&/g, '&amp;')
            .replace(/"/g, '&quot;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;');
    }

    // =========================================================
    // Markdown → HTML 渲染器
    // =========================================================

    /**
     * 将 Markdown 文本渲染为安全的 HTML。
     *
     * 参数:
     * - `text`: Markdown 源文本
     *
     * 返回:
     * - 安全的 HTML 字符串
     *
     * 说明:
     * - 处理流程：代码块保护 → 逐行解析块级元素 → 行内元素解析 → 恢复代码块 → sanitize
     * - 不处理：表格、图片、HTML 标签、脚注、定义列表
     * - 行内代码中的 Markdown 不会被解析（通过占位符保护）
     */
    function render(text) {
        if (!text || typeof text !== 'string') return '';

        // 第一步：保护代码块和行内代码
        var codeBlocks = [];
        var inlineCodes = [];

        // 保护围栏代码块 ```
        text = text.replace(/```(\w*)\n([\s\S]*?)```/g, function (match, lang, code) {
            var idx = codeBlocks.length;
            codeBlocks.push({ lang: lang || '', code: _escHtml(code.trimEnd()) });
            return CODE_PLACEHOLDER_PREFIX + idx + '\n';
        });

        // 保护缩进代码块（4空格或1Tab）
        // 简化处理：在保护围栏代码块后，剩余的缩进块不再特殊处理

        // 保护行内代码
        text = text.replace(/`([^`]+)`/g, function (match, code) {
            var idx = inlineCodes.length;
            inlineCodes.push(_escHtml(code));
            return INLINE_CODE_PREFIX + idx;
        });

        // 第二步：逐行解析块级元素
        var lines = text.split('\n');
        var html = '';
        var inList = null;      // 'ul' | 'ol' | null
        var inBlockquote = false;
        var inParagraph = false;

        for (var i = 0; i < lines.length; i++) {
            var line = lines[i];
            var trimmed = line.trim();

            // 空行 → 关闭所有块
            if (trimmed === '') {
                if (inList) {
                    html += '</' + inList + '>\n';
                    inList = null;
                }
                if (inBlockquote) {
                    html += '</blockquote>\n';
                    inBlockquote = false;
                }
                if (inParagraph) {
                    html += '</p>\n';
                    inParagraph = false;
                }
                continue;
            }

            // 代码块占位符（受保护）
            if (trimmed.indexOf(CODE_PLACEHOLDER_PREFIX) === 0) {
                if (inList) { html += '</' + inList + '>\n'; inList = null; }
                if (inBlockquote) { html += '</blockquote>\n'; inBlockquote = false; }
                if (inParagraph) { html += '</p>\n'; inParagraph = false; }

                var cbIdx = parseInt(trimmed.substring(CODE_PLACEHOLDER_PREFIX.length), 10);
                var cb = codeBlocks[cbIdx];
                if (cb) {
                    var langAttr = cb.lang ? ' class="language-' + _escAttr(cb.lang) + '"' : '';
                    html += '<pre><code' + langAttr + '>' + cb.code + '</code></pre>\n';
                }
                continue;
            }

            // 水平线
            if (/^(-{3,}|\*{3,}|_{3,})\s*$/.test(trimmed)) {
                if (inList) { html += '</' + inList + '>\n'; inList = null; }
                if (inBlockquote) { html += '</blockquote>\n'; inBlockquote = false; }
                if (inParagraph) { html += '</p>\n'; inParagraph = false; }
                html += '<hr />\n';
                continue;
            }

            // 标题
            var headingMatch = trimmed.match(/^(#{1,6})\s+(.+)$/);
            if (headingMatch && !inBlockquote) {
                if (inList) { html += '</' + inList + '>\n'; inList = null; }
                if (inBlockquote) { html += '</blockquote>\n'; inBlockquote = false; }
                if (inParagraph) { html += '</p>\n'; inParagraph = false; }

                var level = Math.min(headingMatch[1].length, 3); // h1-h3 only, cap at h3
                html += '<h' + level + '>' + _parseInline(headingMatch[2], inlineCodes) + '</h' + level + '>\n';
                continue;
            }

            // 无序列表
            var ulMatch = trimmed.match(/^[-*+]\s+(.+)$/);
            if (ulMatch && !inBlockquote) {
                if (inList !== 'ul') {
                    if (inList) html += '</' + inList + '>\n';
                    html += '<ul>\n';
                    inList = 'ul';
                }
                if (inParagraph) { html += '</p>\n'; inParagraph = false; }
                html += '<li>' + _parseInline(ulMatch[1], inlineCodes) + '</li>\n';
                continue;
            }

            // 有序列表
            var olMatch = trimmed.match(/^(\d+)\.\s+(.+)$/);
            if (olMatch && !inBlockquote) {
                if (inList !== 'ol') {
                    if (inList) html += '</' + inList + '>\n';
                    html += '<ol>\n';
                    inList = 'ol';
                }
                if (inParagraph) { html += '</p>\n'; inParagraph = false; }
                html += '<li>' + _parseInline(olMatch[2], inlineCodes) + '</li>\n';
                continue;
            }

            // 块引用
            if (trimmed.startsWith('>')) {
                if (inList) { html += '</' + inList + '>\n'; inList = null; }
                if (inParagraph) { html += '</p>\n'; inParagraph = false; }

                var quoteContent = trimmed.replace(/^>\s?/, '');
                if (!inBlockquote) {
                    html += '<blockquote>\n';
                    inBlockquote = true;
                }
                html += '<p>' + _parseInline(quoteContent, inlineCodes) + '</p>\n';
                continue;
            }

            // 普通段落
            if (!inParagraph && !inBlockquote) {
                if (inList) { html += '</' + inList + '>\n'; inList = null; }
                html += '<p>';
                inParagraph = true;
            } else if (inParagraph) {
                html += '\n';
            }

            html += _parseInline(trimmed, inlineCodes);
        }

        // 关闭未闭合的块
        if (inParagraph) html += '</p>\n';
        if (inBlockquote) html += '</blockquote>\n';
        if (inList) html += '</' + inList + '>\n';

        // 第三步：恢复行内代码占位符
        html = html.replace(new RegExp(INLINE_CODE_PREFIX.replace(/[-\/\\^$*+?.()|[\]{}]/g, '\\$&') + '(\\d+)', 'g'), function (match, idx) {
            var code = inlineCodes[parseInt(idx, 10)];
            return code ? '<code>' + code + '</code>' : '';
        });

        // 第四步：最终 sanitize
        return sanitize(html);
    }

    /**
     * 解析行内 Markdown 元素。
     *
     * 参数:
     * - `text`: 一行文本（不含块级语法）
     * - `inlineCodes`: 行内代码占位符解析表（此版本中已通过占位符保护，故忽略）
     *
     * 返回:
     * - 带行内 HTML 标签的字符串
     */
    function _parseInline(text, inlineCodes) {
        // 先转义 HTML
        text = _escHtml(text);

        // 粗体 + 斜体（***text***）
        text = text.replace(/\*\*\*(.+?)\*\*\*/g, '<strong><em>$1</em></strong>');

        // 粗体（**text**）
        text = text.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');

        // 斜体（*text*）- 注意不要匹配 **
        text = text.replace(/(?<!\*)\*([^*\n]+?)\*(?!\*)/g, '<em>$1</em>');

        // 链接 [text](url)
        text = text.replace(/\[([^\]]+)\]\(([^)]+)\)/g, function (match, linkText, url) {
            var href = _escAttr(url);
            return '<a href="' + href + '" target="_blank" rel="noopener noreferrer">' + linkText + '</a>';
        });

        // 行内代码占位符恢复（在行内解析中可能存在的残余占位符）
        // 注意：inlineCodes 中的行内代码在第二步（逐行解析）之前已通过全局正则保护
        // 这里的 _parseInline 收到的 text 中的 `code` 已被替换为占位符
        // 占位符恢复在 render() 主函数第三步统一处理

        return text;
    }

    /**
     * 仅渲染行内 Markdown（无块级元素）。
     *
     * 参数:
     * - `text`: 行内 Markdown 文本
     *
     * 返回:
     * - 安全的 HTML 字符串（仅含行内标签：strong/em/code/a）
     *
     * 用法:
     *   RamariaMarkdown.renderInline('这是 **粗体** 和 [链接](https://example.com)');
     */
    function renderInline(text) {
        if (!text || typeof text !== 'string') return '';

        // 保护行内代码
        var inlineCodes = [];
        var processed = text.replace(/`([^`]+)`/g, function (match, code) {
            var idx = inlineCodes.length;
            inlineCodes.push(_escHtml(code));
            return INLINE_CODE_PREFIX + idx;
        });

        // 解析行内元素
        var html = _parseInline(processed, inlineCodes);

        // 恢复行内代码
        html = html.replace(new RegExp(INLINE_CODE_PREFIX.replace(/[-\/\\^$*+?.()|[\]{}]/g, '\\$&') + '(\\d+)', 'g'), function (match, idx) {
            var code = inlineCodes[parseInt(idx, 10)];
            return code ? '<code>' + code + '</code>' : '';
        });

        return sanitize(html);
    }

    /**
     * 从 Markdown 中提取纯文本（去除所有格式）。
     *
     * 参数:
     * - `text`: Markdown 源文本
     *
     * 返回:
     * - 纯文本字符串
     *
     * 用法:
     *   var plain = RamariaMarkdown.plainText('**粗体** 和 [链接](url)');
     *   // "粗体 和 链接"
     */
    function plainText(text) {
        if (!text || typeof text !== 'string') return '';

        // 移除代码块
        text = text.replace(/```[\s\S]*?```/g, '');
        // 移除行内代码
        text = text.replace(/`([^`]+)`/g, '$1');
        // 移除链接，保留文本
        text = text.replace(/\[([^\]]+)\]\([^)]+\)/g, '$1');
        // 移除图片
        text = text.replace(/!\[([^\]]*)\]\([^)]+\)/g, '$1');
        // 移除粗体/斜体标记
        text = text.replace(/\*{1,3}([^*\n]+?)\*{1,3}/g, '$1');
        // 移除标题标记
        text = text.replace(/^#{1,6}\s+/gm, '');
        // 移除列表标记
        text = text.replace(/^[-*+]\s+/gm, '');
        text = text.replace(/^\d+\.\s+/gm, '');
        // 移除块引用标记
        text = text.replace(/^>\s?/gm, '');
        // 移除水平线
        text = text.replace(/^[-*_]{3,}\s*$/gm, '');
        // 清理多余空白
        text = text.replace(/\n{3,}/g, '\n\n').trim();

        return text;
    }

    // =========================================================
    // 导出
    // =========================================================

    return {
        render: render,
        renderInline: renderInline,
        sanitize: sanitize,
        plainText: plainText
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaMarkdown', {
    value: RamariaMarkdown,
    writable: false,
    configurable: false,
});
