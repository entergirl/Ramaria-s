/**
 * js/components/toast.js — Ramaria Toast 通知组件
 *
 * 职责:
 * - 全局 Toast 通知管理器（success / warning / error / info 四种类型）
 * - 队列管理：同时最多显示 5 条，超出排队
 * - 自动关闭：可配置 duration（默认 4000ms），hover 暂停计时
 * - 手动关闭：点击 × 按钮立即移除
 * - 使用 store.js 中已定义的 CSS 类，零额外样式编写
 *
 * 设计特点:
 * - 通过 RamariaToast 全局单例访问，无需 import
 * - 容器 #toast-container 由首次调用时自动创建并挂载到 body
 * - 每条 toast 有唯一 id（递增计数器），支持编程式关闭
 * - 事件委托处理关闭按钮点击
 * - 屏幕阅读器友好：role="status" + aria-live="polite"
 *
 * 用法:
 *   RamariaToast.success('操作成功');
 *   RamariaToast.error('连接失败', '请检查网络后重试');
 *   RamariaToast.warning('索引版本过期', '建议重建索引');
 *   RamariaToast.info('正在处理中...');
 *   var id = RamariaToast.show('success', '标题', '消息', { duration: 5000 });
 *   RamariaToast.close(id);
 *
 * 依赖: 无（零外部依赖；需 components.css 提供 .toast-* 样式类）
 */

var RamariaToast = (function () {
    'use strict';

    // =========================================================
    // 常量
    // =========================================================

    /** 默认自动关闭时间（毫秒） */
    var DEFAULT_DURATION = 4000;

    /** 最大同时显示数 */
    var MAX_VISIBLE = 5;

    /** 移除动画时长（毫秒），需与 CSS animation 对齐 */
    var REMOVE_ANIMATION_MS = 200;

    // =========================================================
    // 内部状态
    // =========================================================

    /** toast 自增 id */
    var _nextId = 1;

    /** 当前显示的 toast 列表 [{ id, el, timer, config }] */
    var _activeToasts = [];

    /** 待显示的队列 [{ type, title, message, config }] */
    var _queue = [];

    /** Toast 容器 DOM 元素（懒创建） */
    var _container = null;

    // =========================================================
    // 类型配置
    // =========================================================

    /**
     * 四种类型的图标和 CSS 类映射。
     * - `success`: 绿色对勾
     * - `warning`: 黄色警告三角
     * - `error`: 红色叉号
     * - `info`: 蓝色信息圆圈
     */
    var TYPE_CONFIG = {
        success: {
            icon: '\u2714',   /* ✔ */
            cssClass: 'toast-success',
            defaultTitle: '成功'
        },
        warning: {
            icon: '\u26A0',   /* ⚠ */
            cssClass: 'toast-warning',
            defaultTitle: '警告'
        },
        error: {
            icon: '\u2716',   /* ✖ */
            cssClass: 'toast-error',
            defaultTitle: '错误'
        },
        info: {
            icon: '\u2139',   /* ℹ */
            cssClass: 'toast-info',
            defaultTitle: '提示'
        }
    };

    // =========================================================
    // DOM 操作
    // =========================================================

    /**
     * 获取或创建 Toast 容器。
     *
     * 说明:
     * - 首次调用时创建 <div id="toast-container" class="toast-container"> 并挂载到 body
     * - 设置 role="status" + aria-live="polite" 供屏幕阅读器使用
     * - 后续调用直接返回已有容器
     */
    function _getContainer() {
        if (_container) {
            return _container;
        }

        _container = document.createElement('div');
        _container.id = 'toast-container';
        _container.className = 'toast-container';
        _container.setAttribute('role', 'status');
        _container.setAttribute('aria-live', 'polite');
        _container.setAttribute('aria-atomic', 'false');

        // 事件委托：处理关闭按钮点击
        _container.addEventListener('click', function (e) {
            var closeBtn = e.target.closest('.toast-close');
            if (!closeBtn) return;

            var toastEl = closeBtn.closest('.toast');
            if (!toastEl) return;

            var toastId = _getToastId(toastEl);
            if (toastId !== null) {
                _remove(toastId);
            }
        });

        document.body.appendChild(_container);
        return _container;
    }

    /**
     * 从 DOM 元素上读取 toast id。
     *
     * 参数:
     * - `el`: toast 的 DOM 元素
     *
     * 返回:
     * - 数字 id，或 null（解析失败）
     */
    function _getToastId(el) {
        var raw = el.getAttribute('data-toast-id');
        if (raw === null) return null;
        var id = parseInt(raw, 10);
        return isNaN(id) ? null : id;
    }

    /**
     * 创建并挂载一条 toast DOM。
     *
     * 参数:
     * - `id`: toast 唯一编号
     * - `type`: 'success' | 'warning' | 'error' | 'info'
     * - `title`: 标题文本
     * - `message`: 详细消息（可选，null 时仅显示标题）
     * - `closable`: 是否显示关闭按钮
     *
     * 返回:
     * - 创建的 DOM 元素（已插入容器）
     */
    function _createToastEl(id, type, title, message, closable) {
        var cfg = TYPE_CONFIG[type] || TYPE_CONFIG.info;
        var container = _getContainer();

        var el = document.createElement('div');
        el.className = 'toast ' + cfg.cssClass;
        el.setAttribute('data-toast-id', String(id));
        el.setAttribute('role', 'status');

        // 图标
        var iconSpan = '<span class="toast-icon" aria-hidden="true">' + _escHtml(cfg.icon) + '</span>';

        // 正文
        var bodyHtml = '<div class="toast-body">';
        bodyHtml += '<div class="toast-title">' + _escHtml(title || cfg.defaultTitle) + '</div>';
        if (message) {
            bodyHtml += '<div class="toast-message">' + _escHtml(message) + '</div>';
        }
        bodyHtml += '</div>';

        // 关闭按钮
        var closeHtml = '';
        if (closable) {
            closeHtml = '<button class="toast-close" aria-label="关闭通知" type="button">\u00D7</button>';
        }

        el.innerHTML = iconSpan + bodyHtml + closeHtml;
        container.appendChild(el);

        return el;
    }

    /**
     * 基本的 HTML 转义。
     *
     * 说明:
     * - 仅转义 & < > " ' 五个字符，防止 XSS
     * - 不引入完整 HTML sanitizer（Markdown 渲染走 markdown.js）
     */
    function _escHtml(str) {
        if (typeof str !== 'string') return '';
        return str
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;')
            .replace(/'/g, '&#39;');
    }

    // =========================================================
    // Toast 生命周期
    // =========================================================

    /**
     * 从 DOM 中移除一条 toast（带动画）。
     *
     * 说明:
     * - 给 toast 添加 .removing 类触发 CSS 滑出动画
     * - 动画结束后从 DOM 移除
     * - 清除关联的自动关闭定时器
     * - 从 _activeToasts 移除该项
     * - 检查队列是否有等待项
     */
    function _remove(id) {
        // 查找对应的 active toast
        var index = -1;
        for (var i = 0; i < _activeToasts.length; i++) {
            if (_activeToasts[i].id === id) {
                index = i;
                break;
            }
        }
        if (index === -1) return;

        var item = _activeToasts[index];
        _activeToasts.splice(index, 1);

        // 清除定时器
        if (item.timer) {
            clearTimeout(item.timer);
            item.timer = null;
        }

        // 触发移除动画
        var el = item.el;
        if (el && el.parentNode) {
            el.classList.add('removing');
            setTimeout(function () {
                if (el.parentNode) {
                    el.parentNode.removeChild(el);
                }
            }, REMOVE_ANIMATION_MS);
        }

        // 处理队列
        _drainQueue();
    }

    /**
     * 消费队列中的下一个 toast（如果有）。
     */
    function _drainQueue() {
        if (_queue.length === 0) return;
        if (_activeToasts.length >= MAX_VISIBLE) return;

        var next = _queue.shift();
        _show(next.type, next.title, next.message, next.config);
    }

    /**
     * 创建并显示一条 toast（内部，不检查队列）。
     *
     * 参数:
     * - `type`: 类型字符串
     * - `title`: 标题
     * - `message`: 可选消息体
     * - `config`: { duration, closable }
     */
    function _show(type, title, message, config) {
        config = config || {};

        var id = _nextId++;
        var duration = (typeof config.duration === 'number' && config.duration > 0)
            ? config.duration
            : DEFAULT_DURATION;
        var closable = config.closable !== false;

        var el = _createToastEl(id, type, title, message, closable);

        var item = {
            id: id,
            el: el,
            timer: null,
            config: config
        };

        // 设置自动关闭定时器
        if (duration > 0 && duration !== Infinity) {
            item.timer = setTimeout(function () {
                _remove(id);
            }, duration);
        }

        // hover 时暂停计时
        el.addEventListener('mouseenter', function () {
            if (item.timer) {
                clearTimeout(item.timer);
                item.timer = null;
            }
        });

        el.addEventListener('mouseleave', function () {
            if (!item.timer && duration > 0 && duration !== Infinity) {
                item.timer = setTimeout(function () {
                    _remove(id);
                }, duration);
            }
        });

        _activeToasts.push(item);
        return id;
    }

    // =========================================================
    // 公开 API
    // =========================================================

    /**
     * 显示一条 Toast 通知。
     *
     * 参数:
     * - `type`: 'success' | 'warning' | 'error' | 'info'
     * - `title`: 标题文本（必填）
     * - `message`: 详细消息文本（可选）
     * - `config`: 可选配置
     *     - `duration`: 自动关闭毫秒数（默认 4000，Infinity 不自动关闭）
     *     - `closable`: 是否显示关闭按钮（默认 true）
     *
     * 返回:
     * - toast id（可用于提前关闭）
     *
     * 说明:
     * - 如果当前显示 toast 已达上限（5条），加入队列等待
     * - 队列 FIFO，前面的关闭后自动显示下一个
     */
    function show(type, title, message, config) {
        if (!TYPE_CONFIG[type]) {
            console.warn('[RamariaToast] 未知类型 "' + type + '"，回退为 info');
            type = 'info';
        }

        // 如果已满，加入队列
        if (_activeToasts.length >= MAX_VISIBLE) {
            _queue.push({
                type: type,
                title: title || '',
                message: message || null,
                config: config || {}
            });
            return -1; // 排队中，暂无有效 id
        }

        return _show(type, title, message, config);
    }

    /**
     * 提前关闭指定 toast。
     *
     * 参数:
     * - `id`: show() 返回的 toast id。若传递 -1（排队中），方法静默忽略。
     */
    function close(id) {
        if (id == null || id === -1) return;
        _remove(id);
    }

    /**
     * 关闭所有 toast（显示中的和排队的全部清除）。
     */
    function closeAll() {
        // 清空队列
        _queue.length = 0;

        // 关闭所有活跃 toast（从后往前遍历，避免 splice 干扰）
        var ids = [];
        for (var i = 0; i < _activeToasts.length; i++) {
            ids.push(_activeToasts[i].id);
        }
        for (var j = ids.length - 1; j >= 0; j--) {
            _remove(ids[j]);
        }
    }

    /**
     * 成功通知快捷方法。
     *
     * 用法:
     *   RamariaToast.success('操作完成');
     *   RamariaToast.success('导出成功', '文件已保存到桌面');
     */
    function success(title, message, config) {
        return show('success', title, message, config);
    }

    /**
     * 错误通知快捷方法。
     */
    function error(title, message, config) {
        return show('error', title, message, config);
    }

    /**
     * 警告通知快捷方法。
     */
    function warning(title, message, config) {
        return show('warning', title, message, config);
    }

    /**
     * 信息通知快捷方法。
     */
    function info(title, message, config) {
        return show('info', title, message, config);
    }

    // =========================================================
    // 导出
    // =========================================================

    return {
        show: show,
        close: close,
        closeAll: closeAll,
        success: success,
        error: error,
        warning: warning,
        info: info,

        /**
         * 获取当前活跃 toast 数量（调试用）。
         */
        count: function () {
            return _activeToasts.length;
        },

        /**
         * 获取队列中等待的 toast 数量（调试用）。
         */
        queueCount: function () {
            return _queue.length;
        }
    };
})();
