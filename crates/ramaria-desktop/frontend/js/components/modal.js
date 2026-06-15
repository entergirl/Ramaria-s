/**
 * js/components/modal.js — Ramaria 弹窗组件
 *
 * 职责:
 * - 创建和管理全局 Modal 弹窗（遮罩 + 内容面板）
 * - ESC 键关闭 + 点击遮罩关闭 + 关闭按钮
 * - Focus Trap：焦点在弹窗内循环，不会逃逸到背景元素
 * - 打开弹窗时保存当前焦点，关闭后恢复
 * - 支持 animate-in / animate-out CSS 动画
 *
 * 设计特点:
 * - 通过 RamariaModal 全局单例访问
 * - 使用 components.css 中 .modal-overlay / .modal / .modal-header 等 CSS 类
 * - 使用 inert 属性或 aria-hidden 控制背景可访问性
 * - 弹窗内容支持字符串 HTML 或 DOM 元素
 * - 同一时间仅允许一个弹窗（打开新弹窗前关闭旧弹窗）
 * - 不依赖任何框架或库，纯原生 JS + CSS
 *
 * 用法:
 *   RamariaModal.show({
 *     title: '删除确认',
 *     body: '<p>确定要删除这条记录吗？此操作不可撤销。</p>',
 *     footer: '<button class="btn btn-ghost" data-action="cancel">取消</button>' +
 *             '<button class="btn btn-danger" data-action="confirm">删除</button>',
 *     onAction: function(action) { if (action === 'confirm') { ... } },
 *     size: 'sm'
 *   });
 *   RamariaModal.close();
 *
 * 依赖: 无（零外部依赖；需 components.css 提供 .modal-* 样式类）
 */

var RamariaModal = (function () {
    'use strict';

    // =========================================================
    // 常量
    // =========================================================

    /** 关闭动画时长（毫秒），需与 CSS animation-duration 对齐 */
    var CLOSE_ANIMATION_MS = 160;

    /** 焦点捕获选择器（弹窗内可获得焦点的元素） */
    var FOCUSABLE_SELECTOR = [
        'a[href]',
        'button:not([disabled])',
        'input:not([disabled]):not([type="hidden"])',
        'select:not([disabled])',
        'textarea:not([disabled])',
        '[tabindex]:not([tabindex="-1"])'
    ].join(', ');

    // =========================================================
    // 内部状态
    // =========================================================

    /** 当前打开的弹窗配置 */
    var _current = null;

    /** 打开弹窗前聚焦的元素（用于关闭后恢复焦点） */
    var _previousFocus = null;

    /** 弹窗关闭回调（由 show() 注册，close() 触发时调用） */
    var _onCloseCallback = null;

    // =========================================================
    // DOM 操作
    // =========================================================

    /**
     * 创建弹窗 DOM 结构。
     *
     * 参数:
     * - `config`: show() 传入的配置对象
     *
     * 返回:
     * - { overlay, modal } 两个 DOM 元素的引用
     */
    function _buildModal(config) {
        // 遮罩
        var overlay = document.createElement('div');
        overlay.className = 'modal-overlay';
        overlay.setAttribute('role', 'dialog');
        overlay.setAttribute('aria-modal', 'true');
        overlay.setAttribute('aria-labelledby', 'modal-title');

        // 点击遮罩关闭
        overlay.addEventListener('click', function (e) {
            if (e.target === overlay && config.closeOnOverlay !== false) {
                close();
            }
        });

        // 弹窗面板
        var modal = document.createElement('div');
        modal.className = 'modal';
        if (config.size) {
            modal.classList.add('modal-' + config.size);
        }
        modal.setAttribute('tabindex', '-1');

        // 阻止点击穿透到遮罩
        modal.addEventListener('click', function (e) {
            e.stopPropagation();
        });

        // 头部
        if (config.title) {
            var header = document.createElement('div');
            header.className = 'modal-header';

            var titleEl = document.createElement('h3');
            titleEl.className = 'modal-title';
            titleEl.id = 'modal-title';
            titleEl.textContent = config.title;
            header.appendChild(titleEl);

            // 关闭按钮
            if (config.closable !== false) {
                var closeBtn = document.createElement('button');
                closeBtn.className = 'btn btn-icon btn-sm';
                closeBtn.setAttribute('aria-label', '关闭');
                closeBtn.setAttribute('type', 'button');
                closeBtn.textContent = '\u00D7';
                closeBtn.addEventListener('click', function () {
                    close();
                });
                header.appendChild(closeBtn);
            }

            modal.appendChild(header);
        }

        // 内容区
        var body = document.createElement('div');
        body.className = 'modal-body';
        if (typeof config.body === 'string') {
            body.innerHTML = config.body;
        } else if (config.body instanceof HTMLElement) {
            body.appendChild(config.body);
        }
        modal.appendChild(body);

        // 底部按钮区
        if (config.footer) {
            var footer = document.createElement('div');
            footer.className = 'modal-footer';
            if (typeof config.footer === 'string') {
                footer.innerHTML = config.footer;
            } else if (config.footer instanceof HTMLElement) {
                footer.appendChild(config.footer);
            }

            // 事件委托：处理 footer 中带 data-action 的按钮
            footer.addEventListener('click', function (e) {
                var btn = e.target.closest('[data-action]');
                if (!btn) return;

                var action = btn.getAttribute('data-action');
                if (action && config.onAction) {
                    var preventClose = config.onAction(action, btn);
                    // 除非返回 false 或 'prevent-close'，否则自动关闭
                    if (preventClose !== false && preventClose !== 'prevent-close') {
                        close();
                    }
                }
            });

            modal.appendChild(footer);
        }

        overlay.appendChild(modal);
        return { overlay: overlay, modal: modal };
    }

    // =========================================================
    // 焦点管理
    // =========================================================

    /**
     * 获取弹窗内所有可聚焦元素。
     */
    function _getFocusableElements(modal) {
        if (!modal) return [];
        var elements = modal.querySelectorAll(FOCUSABLE_SELECTOR);
        var result = [];
        for (var i = 0; i < elements.length; i++) {
            var el = elements[i];
            // 跳过隐藏元素
            if (el.offsetParent === null && el.tagName !== 'BODY') continue;
            result.push(el);
        }
        return result;
    }

    /**
     * 将焦点引导到弹窗内第一个可聚焦元素。
     */
    function _focusFirst(modal) {
        var focusable = _getFocusableElements(modal);
        if (focusable.length > 0) {
            focusable[0].focus();
        } else {
            modal.focus();
        }
    }

    /**
     * 焦点陷阱：Tab/Shift+Tab 时焦点在弹窗内循环。
     */
    function _trapFocus(e, modal) {
        if (e.key !== 'Tab') return;

        var focusable = _getFocusableElements(modal);
        if (focusable.length === 0) {
            e.preventDefault();
            return;
        }

        var first = focusable[0];
        var last = focusable[focusable.length - 1];

        if (e.shiftKey) {
            // Shift+Tab
            if (document.activeElement === first) {
                e.preventDefault();
                last.focus();
            }
        } else {
            // Tab
            if (document.activeElement === last) {
                e.preventDefault();
                first.focus();
            }
        }
    }

    /**
     * 设置背景元素为 inert（不可交互）。
     */
    function _setBackgroundInert(disable) {
        var appEl = document.getElementById('app');
        if (appEl) {
            if (disable) {
                appEl.setAttribute('aria-hidden', 'true');
            } else {
                appEl.removeAttribute('aria-hidden');
            }
        }
    }

    // =========================================================
    // ESC 键处理
    // =========================================================

    function _onKeyDown(e) {
        if (e.key === 'Escape' && _current) {
            // 如果弹窗配置了 closeOnEsc: false，则不关闭
            if (_current.closeOnEsc !== false) {
                e.preventDefault();
                close();
            }
            return;
        }

        // 焦点陷阱
        if (_current && _current.modal) {
            _trapFocus(e, _current.modal);
        }
    }

    // =========================================================
    // 公开 API
    // =========================================================

    /**
     * 显示弹窗。
     *
     * 参数:
     * - `config`: 配置对象
     *     - `title`: 标题文本（可选）
     *     - `body`: 内容，字符串 HTML 或 DOM Element
     *     - `footer`: 底部按钮区 HTML 字符串或 DOM Element
     *     - `size`: 'sm' | undefined | 'lg'（默认 480px 宽）
     *     - `closable`: 是否显示右上角关闭按钮（默认 true）
     *     - `closeOnOverlay`: 点击遮罩是否关闭（默认 true）
     *     - `closeOnEsc`: ESC 键是否关闭（默认 true）
     *     - `onAction`: 底部按钮回调 function(action, buttonElement)
     *                   返回 false 或 'prevent-close' 可阻止自动关闭
     *     - `onClose`: 关闭时回调 function()
     *
     * 说明:
     * - 如果已有打开的弹窗，先关闭旧弹窗再打开新的
     * - 打开时保存当前 focus 元素，关闭后恢复
     */
    function show(config) {
        if (!config) {
            console.error('[RamariaModal] show() 需要配置对象');
            return;
        }

        // 先关闭已有弹窗（跳过动画，立即销毁）
        if (_current) {
            _destroy(false);
        }

        // 保存焦点
        _previousFocus = document.activeElement;

        // 构建 DOM
        var parts = _buildModal(config);
        _current = {
            config: config,
            overlay: parts.overlay,
            modal: parts.modal,
            onClose: config.onClose || null
        };

        // 挂载到 body
        document.body.appendChild(parts.overlay);

        // 设置背景不可交互
        _setBackgroundInert(true);

        // 注册全局键盘事件
        document.addEventListener('keydown', _onKeyDown);

        // 焦点进入弹窗
        _focusFirst(parts.modal);

        // 内容中的 data-action 按钮（非 footer 区域）
        parts.modal.addEventListener('click', function (e) {
            var btn = e.target.closest('[data-action]');
            if (!btn) return;

            // 如果按钮在 footer 内，由 footer 的委托处理，这里不重复
            if (btn.closest('.modal-footer')) return;

            var action = btn.getAttribute('data-action');
            if (action && config.onAction) {
                var preventClose = config.onAction(action, btn);
                if (preventClose !== false && preventClose !== 'prevent-close') {
                    close();
                }
            }
        });

        return parts.modal;
    }

    /**
     * 关闭弹窗。
     *
     * 说明:
     * - 触发 CSS closing 动画，动画结束后销毁 DOM
     * - 恢复之前保存的焦点
     * - 调用 onClose 回调
     */
    function close() {
        if (!_current) return;
        _destroy(true);
    }

    /**
     * 销毁弹窗（内部）。
     *
     * 参数:
     * - `animate`: 是否播放关闭动画
     */
    function _destroy(animate) {
        if (!_current) return;

        var overlay = _current.overlay;
        var onClose = _current.onClose;
        var previousFocus = _previousFocus;

        // 清理全局事件
        document.removeEventListener('keydown', _onKeyDown);

        // 恢复背景可交互
        _setBackgroundInert(false);

        // 清除状态
        _current = null;
        _previousFocus = null;

        if (animate && overlay) {
            // 播放关闭动画
            overlay.classList.add('closing');
            var modalEl = overlay.querySelector('.modal');
            if (modalEl) {
                modalEl.classList.add('closing');
            }

            setTimeout(function () {
                if (overlay.parentNode) {
                    overlay.parentNode.removeChild(overlay);
                }
                // 恢复焦点
                _restoreFocus(previousFocus);
                // 触发回调
                if (onClose) {
                    try { onClose(); } catch (e) { console.error('[RamariaModal] onClose 回调出错:', e); }
                }
            }, CLOSE_ANIMATION_MS);
        } else {
            // 立即销毁
            if (overlay && overlay.parentNode) {
                overlay.parentNode.removeChild(overlay);
            }
            _restoreFocus(previousFocus);
            if (onClose) {
                try { onClose(); } catch (e) { console.error('[RamariaModal] onClose 回调出错:', e); }
            }
        }
    }

    /**
     * 恢复焦点到之前保存的元素。
     */
    function _restoreFocus(el) {
        if (el && typeof el.focus === 'function') {
            try {
                el.focus();
            } catch (e) {
                // 元素可能已不存在，静默忽略
            }
        }
    }

    /**
     * 判断当前是否有弹窗打开。
     *
     * 返回:
     * - true: 弹窗正在显示
     */
    function isOpen() {
        return _current !== null;
    }

    /**
     * 获取当前弹窗的 DOM 元素（供外部向弹窗内注入内容）。
     *
     * 返回:
     * - 弹窗内部 .modal 元素，或 null
     */
    function getModalEl() {
        return _current ? _current.modal : null;
    }

    // =========================================================
    // 导出
    // =========================================================

    return {
        show: show,
        close: close,
        isOpen: isOpen,
        getModalEl: getModalEl
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaModal', {
    value: RamariaModal,
    writable: false,
    configurable: false,
});
