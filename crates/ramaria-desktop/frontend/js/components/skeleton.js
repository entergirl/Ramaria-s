/**
 * js/components/skeleton.js — Ramaria 骨架屏组件
 *
 * 职责:
 * - 提供三种骨架屏模板：消息列表、卡片列表、表单
 * - 在数据加载期间展示占位动画，避免空白闪烁
 * - show/hide 方法管理生命周期
 * - 使用 components.css 中 .skeleton / .skeleton-line 等类
 *
 * 设计特点:
 * - 通过 RamariaSkeleton 全局单例访问
 * - 每个模板返回 HTML 字符串，调用方插入到目标容器
 * - show(container, template) 将骨架屏插入容器并返回移除函数
 * - 支持自定义模板（传入 HTML 字符串）
 * - shimmer 动画通过 CSS animation 实现，零 JS 开销
 * - 所有模板均为纯静态占位，不含任何数据依赖
 *
 * 用法:
 *   // 消息列表加载中
 *   var done = RamariaSkeleton.show(document.getElementById('message-list'), 'messages');
 *   fetchMessages().then(function() { done(); });
 *
 *   // 自定义模板
 *   RamariaSkeleton.show(container, '<div class="skeleton" style="height:200px"></div>');
 *
 * 依赖: 无（零外部依赖；需 components.css 提供 .skeleton-* 样式类）
 */

var RamariaSkeleton = (function () {
    'use strict';

    // =========================================================
    // 模板定义
    // =========================================================

    /**
     * 消息列表骨架屏。
     *
     * 说明:
     * - 模拟 4 条对话消息的占位：用户消息右对齐，助手消息左对齐
     * - 每条消息含圆形头像 + 多行文本线
     * - 适合 chat 视图初始化时展示
     */
    function _templateMessages() {
        var html = '';

        // 助手消息 1
        html += '<div class="skeleton-bubble">';
        html += '<div class="skeleton skeleton-avatar md"></div>';
        html += '<div class="skeleton-bubble-body">';
        html += '<div class="skeleton skeleton-line"></div>';
        html += '<div class="skeleton skeleton-line w-80"></div>';
        html += '<div class="skeleton skeleton-line w-60"></div>';
        html += '</div></div>';

        // 用户消息 1
        html += '<div class="skeleton-bubble skeleton-bubble--right">';
        html += '<div class="skeleton skeleton-avatar md"></div>';
        html += '<div class="skeleton-bubble-body">';
        html += '<div class="skeleton skeleton-line w-60"></div>';
        html += '</div></div>';

        // 助手消息 2
        html += '<div class="skeleton-bubble">';
        html += '<div class="skeleton skeleton-avatar md"></div>';
        html += '<div class="skeleton-bubble-body">';
        html += '<div class="skeleton skeleton-line"></div>';
        html += '<div class="skeleton skeleton-line w-80"></div>';
        html += '<div class="skeleton skeleton-line"></div>';
        html += '<div class="skeleton skeleton-line w-40"></div>';
        html += '</div></div>';

        // 用户消息 2
        html += '<div class="skeleton-bubble skeleton-bubble--right">';
        html += '<div class="skeleton skeleton-avatar md"></div>';
        html += '<div class="skeleton-bubble-body">';
        html += '<div class="skeleton skeleton-line w-50"></div>';
        html += '</div></div>';

        return html;
    }

    /**
     * 卡片列表骨架屏。
     *
     * 说明:
     * - 模拟 3 张卡片的占位布局
     * - 每张卡片含图片区 + 标题 + 两行描述
     * - 适合 memory 视图、settings 等卡片式布局
     */
    function _templateCards() {
        var html = '';
        for (var i = 0; i < 3; i++) {
            html += '<div class="skeleton-card">';
            html += '<div class="skeleton skeleton-card-image"></div>';
            html += '<div class="skeleton skeleton-title"></div>';
            html += '<div class="skeleton skeleton-line"></div>';
            html += '<div class="skeleton skeleton-line w-80"></div>';
            html += '</div>';
        }
        return html;
    }

    /**
     * 表单骨架屏。
     *
     * 说明:
     * - 模拟表单字段的占位
     * - 包含标签 + 输入框 + 按钮
     * - 适合 setup 向导、settings 等表单页面
     */
    function _templateForm() {
        var html = '';

        // 字段 1
        html += '<div class="skeleton-form">';
        html += '<div class="skeleton skeleton-line w-30"></div>';
        html += '<div class="skeleton skeleton-line w-full"></div>';
        html += '</div>';

        // 字段 2
        html += '<div class="skeleton-form">';
        html += '<div class="skeleton skeleton-line w-30"></div>';
        html += '<div class="skeleton skeleton-line w-full"></div>';
        html += '</div>';

        // 字段 3（稍短）
        html += '<div class="skeleton-form">';
        html += '<div class="skeleton skeleton-line w-30"></div>';
        html += '<div class="skeleton skeleton-line w-full" style="height:80px"></div>';
        html += '</div>';

        // 按钮区
        html += '<div style="display:flex;gap:8px;margin-top:16px">';
        html += '<div class="skeleton" style="width:100px;height:38px;border-radius:9999px"></div>';
        html += '<div class="skeleton" style="width:120px;height:38px;border-radius:9999px"></div>';
        html += '</div>';

        return html;
    }

    /**
     * 表格骨架屏。
     *
     * 说明:
     * - 模拟数据表格的表头 + 5 行数据
     * - 适合 settings 列表、memory 事件列表等表格视图
     */
    function _templateTable() {
        var html = '';

        // 表头
        html += '<div style="display:flex;gap:12px;padding:10px 12px;border-bottom:2px solid var(--border-light);margin-bottom:8px">';
        html += '<div class="skeleton" style="width:30%;height:14px"></div>';
        html += '<div class="skeleton" style="width:20%;height:14px"></div>';
        html += '<div class="skeleton" style="width:25%;height:14px"></div>';
        html += '<div class="skeleton" style="width:15%;height:14px"></div>';
        html += '</div>';

        // 数据行
        for (var i = 0; i < 5; i++) {
            html += '<div style="display:flex;gap:12px;padding:9px 12px;border-bottom:1px solid var(--border-light)">';
            html += '<div class="skeleton skeleton-line" style="width:30%"></div>';
            html += '<div class="skeleton skeleton-line w-80" style="width:20%"></div>';
            html += '<div class="skeleton skeleton-line w-60" style="width:25%"></div>';
            html += '<div class="skeleton skeleton-line w-40" style="width:15%"></div>';
            html += '</div>';
        }

        return html;
    }

    // =========================================================
    // 模板注册表
    // =========================================================

    var TEMPLATES = {
        messages: _templateMessages,
        cards: _templateCards,
        form: _templateForm,
        table: _templateTable
    };

    // =========================================================
    // 公开 API
    // =========================================================

    /**
     * 在指定容器中显示骨架屏。
     *
     * 参数:
     * - `container`: 目标 DOM 元素（骨架屏将作为其唯一子元素插入）
     * - `template`: 模板名称 'messages' | 'cards' | 'form' | 'table'，或自定义 HTML 字符串
     * - `options`: 可选配置
     *     - `className`: 额外 CSS 类名（加在包裹元素上）
     *
     * 返回:
     * - 清理函数 `done()`，调用后移除骨架屏并恢复容器原始内容
     *
     * 说明:
     * - 调用 show() 前会保存容器原始 innerHTML
     * - 调用 done() 时恢复原始内容
     * - 如果容器不存在或多次调用 show()，最后一次 done() 生效
     */
    function show(container, template, options) {
        if (!container || !(container instanceof HTMLElement)) {
            console.error('[RamariaSkeleton] show() 需要有效的 DOM 容器');
            return function () {};
        }

        options = options || {};

        // 如果容器之前就有骨架屏，先清理
        var existingDone = container._skeletonDone;
        if (typeof existingDone === 'function') {
            existingDone();
        }

        // 保存原始内容
        var originalHTML = container.innerHTML;

        // 生成骨架屏 HTML
        var html;
        if (typeof template === 'function') {
            html = template();
        } else if (typeof template === 'string' && TEMPLATES[template]) {
            html = TEMPLATES[template]();
        } else if (typeof template === 'string') {
            // 自定义 HTML 字符串
            html = template;
        } else {
            console.error('[RamariaSkeleton] 未知模板 "' + template + '"，回退到 messages');
            html = _templateMessages();
        }

        // 包装容器
        var wrapperClass = 'skeleton-wrapper';
        if (options.className) {
            wrapperClass += ' ' + options.className;
        }

        container.innerHTML = '<div class="' + wrapperClass + '">' + html + '</div>';

        // 创建清理函数
        var cleaned = false;
        var done = function () {
            if (cleaned) return;
            cleaned = true;

            // 恢复原始内容
            container.innerHTML = originalHTML;

            // 清理引用
            delete container._skeletonDone;
        };

        // 存储清理函数，防止重复调用
        container._skeletonDone = done;

        return done;
    }

    /**
     * 创建骨架屏 HTML 字符串（不插入 DOM）。
     *
     * 参数:
     * - `template`: 模板名称或自定义 HTML
     *
     * 返回:
     * - HTML 字符串
     *
     * 用法:
     *   var html = RamariaSkeleton.render('cards');
     *   element.innerHTML = html;
     */
    function render(template) {
        if (typeof template === 'function') {
            return template();
        }
        if (typeof template === 'string' && TEMPLATES[template]) {
            return TEMPLATES[template]();
        }
        if (typeof template === 'string') {
            return template;
        }
        return _templateMessages();
    }

    // =========================================================
    // 导出
    // =========================================================

    return {
        show: show,
        render: render
    };
})();
