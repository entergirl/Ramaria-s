/**
 * js/components/message-bubble.js — Ramaria 消息气泡组件
 *
 * 职责:
 * - 渲染单条消息气泡（用户右对齐粉色 / 助手左对齐蓝色）
 * - 支持 Markdown 内容渲染（RamariaMarkdown）
 * - 支持流式更新：增量追加文本、显示打字光标
 * - 支持时间戳和人格标注显示
 *
 * 设计特点:
 * - 工厂函数 createBubble(msg) 返回气泡 DOM 元素
 * - 通过 data-message-id 属性标记，支持后续查找和更新
 * - 打字光标使用 CSS animation（typing-cursor 类，由 animations.css 定义），零 JS 定时器
 * - 消息气泡入场动画（fadeInUp）由 chat.css 的 .msg-bubble-wrapper 驱动
 * - 角色映射：user → 右对齐粉底 / assistant → 左对齐蓝底 / system → 居中灰底
 * - role 仅内部使用，不随 Markdown 内容暴露给用户
 * 
 * 用法:
 *   var bubble = RamariaMessageBubble.create({ id, role, content, persona_uid, created_at });
 *   var bubble = RamariaMessageBubble.createStreaming({ id: 'temp', role: 'assistant' });
 *   RamariaMessageBubble.updateContent('temp', '新增内容');
 *   RamariaMessageBubble.finalize('temp', finalContent);
 *
 * 依赖:
 * - RamariaMarkdown（js/utils/markdown.js）
 * - RamariaFormat（js/utils/format.js）
 * - CSS: 内联使用 tokens.css 变量
 */

var RamariaMessageBubble = (function () {
    'use strict';

    // =========================================================
    // 常量
    // =========================================================

    /** 角色样式映射 */
    var ROLE_STYLES = {
        user: {
            align: 'flex-end',
            bg: 'oklch(0.53 0.19 10 / 0.08)',
            border: 'oklch(0.53 0.19 10 / 0.18)',
            label: '你',
            labelColor: 'var(--pink-500)',
        },
        assistant: {
            align: 'flex-start',
            bg: 'oklch(0.48 0.17 225 / 0.06)',
            border: 'oklch(0.48 0.17 225 / 0.14)',
            label: '助手',
            labelColor: 'var(--blue-500)',
        },
        system: {
            align: 'center',
            bg: 'var(--bg-subtle)',
            border: 'var(--border-default)',
            label: '系统',
            labelColor: 'var(--text-tertiary)',
        },
    };

    // =========================================================
    // 工厂函数
    // =========================================================

    /**
     * 创建一个标准消息气泡。
     *
     * 参数:
     * - `msg`: { id, role, content, persona_uid?, created_at? }
     *
     * 返回:
     * - DOM 元素（.message-bubble-wrapper），可直接插入消息列表
     */
    function create(msg) {
        if (!msg || !msg.role) {
            console.error('[MessageBubble] create 需要 msg.role');
            return _createPlaceholder('消息数据异常');
        }

        var style = ROLE_STYLES[msg.role] || ROLE_STYLES.system;
        var wrapper = document.createElement('div');
        wrapper.className = 'msg-bubble-wrapper';
        wrapper.setAttribute('data-message-id', msg.id || '');
        wrapper.setAttribute('data-role', msg.role);
        wrapper.style.cssText =
            'display:flex;flex-direction:column;align-items:' + style.align + ';' +
            'margin-bottom:var(--space-3);padding:0 var(--space-4);';

        // 角色标签 + 时间戳
        var metaHtml = '';
        if (msg.role !== 'system') {
            var timeStr = msg.created_at ? RamariaFormat.smartTime(msg.created_at) : '';
            metaHtml =
                '<div style="display:flex;align-items:center;gap:var(--space-2);margin-bottom:var(--space-1);' +
                'font-size:11px;">' +
                '<span style="color:' + style.labelColor + ';font-weight:500;">' + style.label + '</span>' +
                (msg.persona_uid
                    ? '<span style="color:var(--text-tertiary);" title="人格: ' + msg.persona_uid + '">' +
                      '@' + msg.persona_uid + '</span>'
                    : '') +
                (timeStr ? '<span style="color:var(--text-tertiary);">' + timeStr + '</span>' : '') +
                '</div>';
        } else {
            metaHtml =
                '<div style="text-align:center;font-size:10px;color:var(--text-tertiary);margin-bottom:var(--space-1);">' +
                (msg.created_at ? RamariaFormat.smartTime(msg.created_at) : '') +
                '</div>';
        }

        // 气泡内容
        var bubble = document.createElement('div');
        bubble.className = 'msg-bubble';
        bubble.style.cssText =
            'max-width:75%;padding:var(--space-3) var(--space-4);' +
            'border-radius:var(--radius-md);' +
            'background:' + style.bg + ';' +
            'border:1px solid ' + style.border + ';' +
            'font-size:13.5px;line-height:1.65;' +
            'color:var(--text-primary);' +
            'word-break:break-word;overflow-wrap:break-word;';

        // Markdown 渲染
        var contentHtml = '';
        try {
            contentHtml = RamariaMarkdown.render(msg.content || '');
        } catch (err) {
            console.error('[MessageBubble] Markdown 渲染失败:', err);
            contentHtml = RamariaMarkdown.sanitize
                ? RamariaMarkdown.sanitize(msg.content || '')
                : _escHtml(msg.content || '');
        }
        bubble.innerHTML = contentHtml;

        wrapper.innerHTML = metaHtml;
        wrapper.appendChild(bubble);

        return wrapper;
    }

    /**
     * 创建流式消息气泡（初始化空内容，带打字光标）。
     *
     * 参数:
     * - `opts`: { id, role (默认 'assistant') }
     *
     * 返回:
     * - DOM 元素
     */
    function createStreaming(opts) {
        opts = opts || {};
        var role = opts.role || 'assistant';
        var style = ROLE_STYLES[role] || ROLE_STYLES.assistant;
        var id = opts.id || ('streaming-' + Date.now());

        var wrapper = document.createElement('div');
        wrapper.className = 'msg-bubble-wrapper';
        wrapper.setAttribute('data-message-id', id);
        wrapper.setAttribute('data-role', role);
        wrapper.setAttribute('data-streaming', 'true');
        wrapper.style.cssText =
            'display:flex;flex-direction:column;align-items:' + style.align + ';' +
            'margin-bottom:var(--space-3);padding:0 var(--space-4);';

        // 角色标签
        wrapper.innerHTML =
            '<div style="display:flex;align-items:center;gap:var(--space-2);margin-bottom:var(--space-1);' +
            'font-size:11px;">' +
            '<span style="color:' + style.labelColor + ';font-weight:500;">' + style.label + '</span>' +
            '<span style="color:var(--text-tertiary);">正在生成...</span>' +
            '</div>';

        // 气泡内容
        var bubble = document.createElement('div');
        bubble.className = 'msg-bubble msg-bubble--streaming';
        bubble.style.cssText =
            'max-width:75%;padding:var(--space-3) var(--space-4);' +
            'border-radius:var(--radius-md);' +
            'background:' + style.bg + ';' +
            'border:1px solid ' + style.border + ';' +
            'font-size:13.5px;line-height:1.65;' +
            'color:var(--text-primary);' +
            'word-break:break-word;overflow-wrap:break-word;' +
            'position:relative;';
        bubble.innerHTML =
            '<span class="msg-bubble-text"></span>' +
            '<span class="typing-cursor" aria-hidden="true"></span>';

        wrapper.appendChild(bubble);

        return wrapper;
    }

    /**
     * 向流式气泡追加内容。
     *
     * 参数:
     * - `msgId`: 消息 ID（与 createStreaming 中的 opts.id 对应）
     * - `delta`: 增量文本
     *
     * 说明:
     * - 通过 data-message-id 查找气泡
     * - 追加内容到 .msg-bubble-text span
     * - 如果未找到气泡，静默忽略（可能 DOM 已被移除）
     */
    function updateContent(msgId, delta) {
        if (!delta) return;

        var wrapper = document.querySelector('.msg-bubble-wrapper[data-message-id="' + msgId + '"]');
        if (!wrapper) return;

        var textEl = wrapper.querySelector('.msg-bubble-text');
        if (!textEl) return;

        textEl.textContent += delta;
    }

    /**
     * 完成流式气泡（移除打字光标，渲染为最终 Markdown）。
     *
     * 参数:
     * - `msgId`: 消息 ID
     * - `finalContent`: 最终完整内容
     * - `createdAt`: 可选，完成时间戳
     *
     * 说明:
     * - 移除 typing cursor CSS 和 data-streaming 属性
     * - 将文本内容替换为 Markdown 渲染结果
     * - 更新元数据（时间戳、"正在生成..." → 实际时间）
     */
    function finalize(msgId, finalContent, createdAt) {
        var wrapper = document.querySelector('.msg-bubble-wrapper[data-message-id="' + msgId + '"]');
        if (!wrapper) return;

        // 移除流式标记
        wrapper.removeAttribute('data-streaming');

        // 更新气泡内容为 Markdown
        var bubble = wrapper.querySelector('.msg-bubble');
        if (bubble) {
            bubble.classList.remove('msg-bubble--streaming');
            try {
                bubble.innerHTML = RamariaMarkdown.render(finalContent || '');
            } catch (err) {
                console.error('[MessageBubble] finalize Markdown 渲染失败:', err);
                bubble.textContent = finalContent || '';
            }
        }

        // 更新时间戳
        if (createdAt) {
            var metaSpan = wrapper.querySelector('span');
            if (metaSpan) {
                // 找到"正在生成..."并替换
                var children = wrapper.querySelectorAll('span');
                for (var i = 0; i < children.length; i++) {
                    if (children[i].textContent === '正在生成...') {
                        children[i].textContent = RamariaFormat.smartTime(createdAt);
                        break;
                    }
                }
            }
        }
    }

    /**
     * 为消息气泡标记错误状态。
     *
     * 参数:
     * - `msgId`: 消息 ID
     * - `errorText`: 错误描述文本
     */
    function markError(msgId, errorText) {
        var wrapper = document.querySelector('.msg-bubble-wrapper[data-message-id="' + msgId + '"]');
        if (!wrapper) return;

        wrapper.removeAttribute('data-streaming');

        var bubble = wrapper.querySelector('.msg-bubble');
        if (bubble) {
            bubble.classList.remove('msg-bubble--streaming');
            bubble.style.borderColor = 'var(--pink-400)';
        }

        // 追加错误提示
        var errorEl = document.createElement('div');
        errorEl.style.cssText =
            'margin-top:var(--space-2);font-size:11px;color:var(--pink-500);' +
            'display:flex;align-items:center;gap:var(--space-1);';
        // 使用 textContent 防止 LLM 返回的 HTML 特殊字符被注入执行
        errorEl.textContent = '⚠️ ' + (errorText || '生成失败');
        wrapper.appendChild(errorEl);
    }

    // =========================================================
    // 辅助函数
    // =========================================================

    function _escHtml(text) {
        var div = document.createElement('div');
        div.appendChild(document.createTextNode(text));
        return div.innerHTML;
    }

    function _createPlaceholder(text) {
        var el = document.createElement('div');
        el.className = 'msg-bubble-wrapper';
        el.style.cssText =
            'text-align:center;padding:var(--space-4);color:var(--text-tertiary);font-size:12px;';
        el.textContent = text;
        return el;
    }

    // =========================================================
    // 公开 API
    // =========================================================

    return {
        create: create,
        createStreaming: createStreaming,
        updateContent: updateContent,
        finalize: finalize,
        markError: markError,
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaMessageBubble', {
    value: RamariaMessageBubble,
    writable: false,
    configurable: false,
});
