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
 * - 助手气泡左侧显示人格头像（首字母圆形），用户气泡右侧无头像
 * - persona_uid 为 null/空时按角色回退标签（user→"你"，assistant→"助手"）
 * - 多气泡：助手回复按 `||` 契约拆分为多条气泡（历史回读与流式收尾同一拆分口径）
 *
 * 用法:
 * var bubble = RamariaMessageBubble.create({ id, role, content, persona_uid, created_at });
 * var bubble = RamariaMessageBubble.createStreaming({ id: 'temp', role: 'assistant' });
 * RamariaMessageBubble.updateStreamText('temp', '已累积的全文');
 * RamariaMessageBubble.finalize('temp', finalContent);
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
 * 导入时 parser.rs 的 make_role_content 在 content 前拼接了
 * `[{sender_name}] ` 格式的前缀。
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
// 空值/空串统一回退占位：避免渲染出不可见的空气泡（内容缺失要显式可见）
        if (!content) return '[空消息]';
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

 // ── 角色标签逻辑 ──
 // assistant 消息带 persona_uid → 显示 persona 昵称（对话人，在左侧）
 // assistant 消息无 persona_uid → 回退 "助手"
 // user 消息 → 始终显示 "你"（用户自己，在右侧）
        var personaName = '';
        var label;
        if (role === 'assistant' && msg.persona_uid) {
            personaName = _lookupPersonaName(msg.persona_uid);
            label = personaName || '助手';
        } else {
            label = ROLE_LABELS[role] || ROLE_LABELS.system;
        }

 // ── 内容分段：助手回复按 `||` 契约拆分为多条气泡；其它角色整段单泡 ──
// 每条气泡各自剥离导入消息的 [{name}] 前缀（纯展示层）
        var rawContent = msg.content || '';
        var segments = role === 'assistant'
            ? _splitAssistantBubbles(rawContent)
            : [_stripImportPrefix(rawContent)];

 // wrapper
        var wrapper = document.createElement('div');
        wrapper.className = 'msg-bubble-wrapper';
        wrapper.setAttribute('data-message-id', msg.id || '');
        wrapper.setAttribute('data-role', role);

 // ── 助手气泡左侧显示人格头像 ──
        if (role === 'assistant' && personaName) {
            var avatarEl = document.createElement('div');
            avatarEl.className = 'msg-bubble-avatar';
            avatarEl.setAttribute('aria-hidden', 'true');
            avatarEl.textContent = personaName.charAt(0).toUpperCase();
 // 稳定的头像背景色（由 persona_uid hash 决定）
            avatarEl.style.backgroundColor = _avatarColor(msg.persona_uid || '');
            wrapper.appendChild(avatarEl);
        }

 // 元数据行（角色标签 + 人格 + 时间戳）
        if (role !== 'system') {
            var meta = document.createElement('div');
            meta.className = 'msg-bubble-meta';

            var labelSpan = document.createElement('span');
            labelSpan.className = 'msg-bubble-label';
            labelSpan.textContent = label;
            meta.appendChild(labelSpan);

 // 仅 assistant 消息且 persona 非 rama-0001 时显示 @persona 标注
            if (role === 'assistant' && msg.persona_uid && personaName && msg.persona_uid.indexOf('rama-0001') !== 0) {
                var personaSpan = document.createElement('span');
                personaSpan.className = 'msg-bubble-persona';
                personaSpan.title = '人格: ' + personaName;
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

 // 气泡内容（助手回复按契约拆分为多条气泡；其它角色单条）
        for (var b = 0; b < segments.length; b++) {
            wrapper.appendChild(_createBubbleEl(segments[b]));
        }

        return wrapper;
    }

/**
 * 将助手回复文本按 `||` 契约拆分为展示段（每段各自剥离导入前缀）。
 *
 * 说明:
 * - 分隔符契约与提示词「核心规则」一致；`RamariaBubble` 不可用时退化为单段。
 * - 无有效分段（纯分隔符/空白）时回退**原文单泡**：宁可原样展示，也不丢内容、
 *   不渲染空气泡（CR2-COR-011）；真正的空内容由 [_stripImportPrefix] 产出占位。
 */
    function _splitAssistantBubbles(content) {
        var raw = (typeof RamariaBubble !== 'undefined' && RamariaBubble.splitBubbles)
            ? RamariaBubble.splitBubbles(content)
            : [content];
        if (raw.length === 0) raw = [content];

        var out = [];
        for (var i = 0; i < raw.length; i++) {
            out.push(_stripImportPrefix(raw[i]));
        }
        return out;
    }

/**
 * 创建单个气泡元素（Markdown 渲染 + 异常兜底）。
 */
    function _createBubbleEl(text) {
        var bubble = document.createElement('div');
        bubble.className = 'msg-bubble';

        try {
            bubble.innerHTML = RamariaMarkdown.render(text);
        } catch (err) {
            console.error('[MessageBubble] Markdown 渲染失败:', err);
            bubble.innerHTML = RamariaMarkdown.sanitize
                ? RamariaMarkdown.sanitize(text)
                : _escHtml(text);
        }

        return bubble;
    }

/**
 * 重建 wrapper 内的气泡（流式收尾：整段替换为按契约拆分的多条气泡）。
 */
    function _replaceBubbles(wrapper, content) {
        var old = wrapper.querySelectorAll('.msg-bubble');
        for (var i = 0; i < old.length; i++) {
            wrapper.removeChild(old[i]);
        }

        var segments = _splitAssistantBubbles(content);
        for (var j = 0; j < segments.length; j++) {
            wrapper.appendChild(_createBubbleEl(segments[j]));
        }
    }

 /**
 * 根据 uid 生成稳定的头像背景色。
 */
    function _avatarColor(uid) {
        if (!uid) return '#9ca3af';
        var hash = 0;
        for (var i = 0; i < uid.length; i++) {
            hash = uid.charCodeAt(i) + ((hash << 5) - hash);
        }
        var hue = Math.abs(hash) % 360;
        return 'hsl(' + hue + ', 40%, 55%)';
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
 * 用“已累积全文快照”刷新流式气泡文本。
 *
 * 参数:
 * - `msgId`: 消息 ID（与 createStreaming 中的 opts.id 对应）
 * - `fullText`: 到目前为止的完整回复文本（非增量）
 *
 * 说明:
 * - 通过 data-message-id 查找气泡；找不到（视图已切换/气泡被移除）时静默忽略，
 *   调用方（chat.js）仍持有全文，chat-done 时会用完整内容补齐。
 * - 流式期间把契约分隔符 `||` 临时渲染为换行（CR2-COR-012）：
 *   避免生成过程中把 `||` 原样暴露给用户、finalize 时再“跳变”重排；
 *   代码块/行内代码/URL 内的 `||` 由 RamariaBubble.streamDisplay 豁免，保持原样。
 * - 使用 textContent 写入纯文本（不做 Markdown 渲染，避免半截语法抖动）。
 */
    function updateStreamText(msgId, fullText) {
        if (typeof fullText !== 'string') return;

        var wrapper = document.querySelector('.msg-bubble-wrapper[data-message-id="' + msgId + '"]');
        if (!wrapper) return;

        var textEl = wrapper.querySelector('.msg-bubble-text');
        if (!textEl) return;

        var display = (typeof RamariaBubble !== 'undefined' && RamariaBubble.streamDisplay)
            ? RamariaBubble.streamDisplay(fullText)
            : fullText.replace(/\|\|/g, '\n');

        textEl.textContent = display;
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

 // 按 `||` 契约重建为多条气泡（与历史回读同一拆分口径）
         _replaceBubbles(wrapper, finalContent || '');

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
        updateStreamText: updateStreamText,
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
