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
 * - CSP-safe: 全部样式走 CSS 类，零内联 style（包括 innerHTML 中的 style 属性）
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
 * - CSS: chat.css（消息气泡样式）
 */

var RamariaMessageBubble = (function () {
    'use strict';

    // =========================================================
    // 常量
    // =========================================================

    /** 角色标签文案（回退值，当无法解析 persona name 时使用） */
    var ROLE_LABELS = {
        user: '你',
        assistant: '助手',
        system: '系统',
    };

    // =========================================================
    // 辅助函数
    // =========================================================

    /**
     * 从 Store 缓存的 persona 列表中查找 persona 名称。
     *
     * 参数:
     * - `personaUid`: persona 业务标识（如 "char-123456789"）
     *
     * 返回:
     * - persona 的 `name` 字段；找不到则返回空字符串。
     *
     * 说明:
     * - 用于气泡元数据行中显示发送者真实昵称，替代硬编码的 "你"/"助手"。
     * - 仅在 `persona_uid` 存在且非 `rama-0001`（默认 AI）时尝试解析。
     */
    function _lookupPersonaName(personaUid) {
        if (!personaUid || !RamariaStore) return '';
        try {
            var personas = RamariaStore.get('personas') || [];
            for (var i = 0; i < personas.length; i++) {
                if (personas[i].uid === personaUid) {
                    return personas[i].name || '';
                }
            }
        } catch (_) { /* ignore */ }
        return '';
    }

    /**
     * 剥离导入消息的 [{name}] 前缀（纯展示层）。
     *
     * 导入时 parser.rs 的 make_role_content() 在 content 前拼接了
     * `[{sender_name}] ` 格式的前缀（v2.1 双前缀模式）。
     * 此函数在渲染前剥离该前缀，避免对话框中重复显示昵称。
     *
     * 参数:
     * - `content`: 原始消息内容
     *
     * 返回:
     * - 剥离前缀后的内容；若内容仅剩空白则返回 "[空消息]"。
     *
     * 说明:
     * - 不修改数据库内容，保持 L1 摘要可访问完整上下文。
     * - 正常 AI 对话不会产生 `[{name}] ` 前缀，此操作安全无副作用。
     */
    function _stripImportPrefix(content) {
        if (!content) return '';
        // 匹配行首的 [{任意字符}] 后跟可选空格
        var stripped = content.replace(/^\[[^\]]+\]\s*/, '');
        // 极端情况：消息本身只有前缀无正文
        if (!stripped.trim()) return '[空消息]';
        return stripped;
    }

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
     * - DOM 元素（.msg-bubble-wrapper），可直接插入消息列表
     */
    function create(msg) {
        if (!msg || !msg.role) {
            console.error('[MessageBubble] create 需要 msg.role');
            return _createPlaceholder('消息数据异常');
        }

        var role = msg.role;

        // ── v1.1 修复: 角色标签优先使用 persona name ──
        var personaName = _lookupPersonaName(msg.persona_uid);
        var label;
        if (personaName && msg.persona_uid && msg.persona_uid.indexOf('rama-0001') !== 0) {
            // 导入的 persona 或非默认 AI —— 使用真实昵称
            label = personaName;
        } else {
            // 回退到硬编码标签
            label = ROLE_LABELS[role] || ROLE_LABELS.system;
        }

        // ── v1.1 修复: 剥离导入消息的 [{name}] 前缀（纯展示层）──
        var displayContent = _stripImportPrefix(msg.content || '');

        // wrapper
        var wrapper = document.createElement('div');
        wrapper.className = 'msg-bubble-wrapper';
        wrapper.setAttribute('data-message-id', msg.id || '');
        wrapper.setAttribute('data-role', role);

        // 元数据行（角色标签 + 人格 + 时间戳）
        if (role !== 'system') {
            var meta = document.createElement('div');
            meta.className = 'msg-bubble-meta';

            var labelSpan = document.createElement('span');
            labelSpan.className = 'msg-bubble-label';
            labelSpan.textContent = label;
            meta.appendChild(labelSpan);

            if (msg.persona_uid) {
                var personaSpan = document.createElement('span');
                personaSpan.className = 'msg-bubble-persona';
                // 如果已解析出 persona name，使用 name 作为 title；否则显示 uid
                var personaDisplay = personaName || msg.persona_uid;
                personaSpan.title = '人格: ' + personaDisplay;
                personaSpan.textContent = '@' + personaDisplay;
                meta.appendChild(personaSpan);
            }

            if (msg.created_at) {
                var timeSpan = document.createElement('span');
                timeSpan.className = 'msg-bubble-time';
                timeSpan.textContent = RamariaFormat.smartTime(msg.created_at);
                meta.appendChild(timeSpan);
            }

            wrapper.appendChild(meta);
        } else {
            var sysMeta = document.createElement('div');
            sysMeta.className = 'msg-bubble-meta--system';
            if (msg.created_at) {
                sysMeta.textContent = RamariaFormat.smartTime(msg.created_at);
            }
            wrapper.appendChild(sysMeta);
        }

        // 气泡内容（使用剥离前缀后的 displayContent）
        var bubble = document.createElement('div');
        bubble.className = 'msg-bubble';

        try {
            bubble.innerHTML = RamariaMarkdown.render(displayContent);
        } catch (err) {
            console.error('[MessageBubble] Markdown 渲染失败:', err);
            bubble.innerHTML = RamariaMarkdown.sanitize
                ? RamariaMarkdown.sanitize(displayContent)
                : _escHtml(displayContent);
        }

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
        var label = ROLE_LABELS[role] || ROLE_LABELS.assistant;
        var id = opts.id || ('streaming-' + Date.now());

        var wrapper = document.createElement('div');
        wrapper.className = 'msg-bubble-wrapper';
        wrapper.setAttribute('data-message-id', id);
        wrapper.setAttribute('data-role', role);
        wrapper.setAttribute('data-streaming', 'true');

        // 元数据行
        var meta = document.createElement('div');
        meta.className = 'msg-bubble-meta';

        var labelSpan = document.createElement('span');
        labelSpan.className = 'msg-bubble-label';
        labelSpan.textContent = label;
        meta.appendChild(labelSpan);

        var streamingSpan = document.createElement('span');
        streamingSpan.className = 'msg-bubble-streaming-label';
        streamingSpan.textContent = '正在生成...';
        meta.appendChild(streamingSpan);

        wrapper.appendChild(meta);

        // 气泡内容（流式）
        var bubble = document.createElement('div');
        bubble.className = 'msg-bubble msg-bubble--streaming';
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
     * - 更新元数据（"正在生成..." → 实际时间）
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
            var streamingLabels = wrapper.querySelectorAll('.msg-bubble-streaming-label');
            for (var i = 0; i < streamingLabels.length; i++) {
                streamingLabels[i].classList.remove('msg-bubble-streaming-label');
                streamingLabels[i].classList.add('msg-bubble-time');
                streamingLabels[i].textContent = RamariaFormat.smartTime(createdAt);
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
            bubble.classList.add('msg-bubble--error');
        }

        // 追加错误提示
        var errorEl = document.createElement('div');
        errorEl.className = 'msg-bubble-error';
        // 使用 textContent 防止 LLM 返回的 HTML 特殊字符被注入执行
        errorEl.textContent = '\u26A0\uFE0F ' + (errorText || '生成失败');
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
        el.className = 'msg-bubble-placeholder';
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
