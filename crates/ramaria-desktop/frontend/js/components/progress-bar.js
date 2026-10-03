/**
 * components/progress-bar.js — 非阻塞进度条组件
 *
 * 职责:
 * - 在对话页顶部显示嵌入模型下载/索引重建进度条
 * - 进度内容由调用方经 show / showError / hide 显式驱动
 * - 进度条为非阻塞设计，不阻止用户继续对话
 * - 支持百分比、速度/文档数显示
 *
 * 设计特点:
 * - 纯 JS，无外部依赖
 * - 自动注入 DOM，无需修改现有 HTML
 * - 错误状态 5 秒后自动隐藏
 * - 错误时显示红色警告
 *
 * 用法:
 * RamariaProgressBar.init; // 在 chat view enter 钩子中调用
 */
var RamariaProgressBar = (function () {
    'use strict';

    var _container = null;
    var _bar = null;
    var _text = null;
    var _detail = null;
    var _initialized = false;
    var _hideTimer = null;

 // =========================================================
 // DOM 构建
 // =========================================================

    function _createDom() {
 // 查找对话视图的内容区头部
        var contentHeader = document.querySelector('.content-header');
        if (!contentHeader) {
            console.warn('[ProgressBar] 未找到 .content-header 元素');
            return;
        }

 // 创建进度条容器
        _container = document.createElement('div');
        _container.id = 'inline-progress-bar';
        _container.className = 'inline-progress-bar hidden';
        _container.setAttribute('role', 'progressbar');
        _container.setAttribute('aria-valuemin', '0');
        _container.setAttribute('aria-valuemax', '100');
        _container.setAttribute('aria-valuenow', '0');

 // 进度文本行
        var row = document.createElement('div');
        row.className = 'inline-progress-row';

        _text = document.createElement('span');
        _text.className = 'inline-progress-text';
        _text.textContent = '';

        _detail = document.createElement('span');
        _detail.className = 'inline-progress-detail';
        _detail.textContent = '';

        row.appendChild(_text);
        row.appendChild(_detail);

 // 进度条轨道
        var track = document.createElement('div');
        track.className = 'inline-progress-track';

        _bar = document.createElement('div');
        _bar.className = 'inline-progress-fill';
        _bar.style.width = '0%';

        track.appendChild(_bar);

        _container.appendChild(row);
        _container.appendChild(track);

 // 插入到 content-header 之后
        contentHeader.parentNode.insertBefore(_container, contentHeader.nextSibling);
    }

 // =========================================================
 // 进度更新
 // =========================================================

 /**
 * 显示进度条并更新状态。
 *
 * 参数:
 * - `title`: 进度标题（如 "正在下载嵌入模型"）
 * - `detail`: 详细信息（如 "45% · 12.3 MB / 27.3 MB"）
 * - `percent`: 进度百分比 0..100（-1 表示不确定）
 */
    function show(title, detail, percent) {
        if (!_container) {
            console.warn('[ProgressBar] 容器未初始化');
            return;
        }

 // 清除自动隐藏定时器
        if (_hideTimer) {
            clearTimeout(_hideTimer);
            _hideTimer = null;
        }

        _container.classList.remove('hidden', 'error');
        _text.textContent = title || '';
        _detail.textContent = detail || '';

        if (percent >= 0 && percent <= 100) {
            _bar.style.width = percent + '%';
            _container.setAttribute('aria-valuenow', String(Math.round(percent)));
        } else {
 // 不确定进度（动画）
            _bar.style.width = '30%';
            _bar.style.animation = 'progress-indeterminate 2s ease-in-out infinite';
            _container.setAttribute('aria-valuenow', '0');
        }
    }

 /**
 * 显示错误状态（红色）。
 */
    function showError(title, detail) {
        if (!_container) return;

        if (_hideTimer) {
            clearTimeout(_hideTimer);
            _hideTimer = null;
        }

        _container.classList.remove('hidden');
        _container.classList.add('error');
        _text.textContent = title || '操作失败';
        _detail.textContent = detail || '';

 // 5 秒后自动隐藏
        _hideTimer = setTimeout(hide, 5000);
    }

 /**
 * 隐藏进度条。
 */
    function hide() {
        if (_container) {
            _container.classList.add('hidden');
            _container.classList.remove('error');
            _bar.style.width = '0%';
            _bar.style.animation = '';
            _text.textContent = '';
            _detail.textContent = '';
        }
        if (_hideTimer) {
            clearTimeout(_hideTimer);
            _hideTimer = null;
        }
    }

 // =========================================================
 // 初始化与销毁
 // =========================================================

    function init() {
        if (_initialized) {
            console.warn('[ProgressBar] 已初始化，跳过');
            return;
        }

        console.log('[ProgressBar] 初始化进度条组件...');

        _createDom();

        _initialized = true;
        console.log('[ProgressBar] 进度条组件初始化完成');
    }

    function destroy() {
        console.log('[ProgressBar] 销毁进度条组件...');

        hide();

        if (_container && _container.parentNode) {
            _container.parentNode.removeChild(_container);
        }
        _container = null;
        _bar = null;
        _text = null;
        _detail = null;
        _initialized = false;
    }

 // =========================================================
 // 公开 API
 // =========================================================

    return {
        init: init,
        destroy: destroy,
        show: show,
        showError: showError,
        hide: hide,
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaProgressBar', {
    value: RamariaProgressBar,
    writable: false,
    configurable: false,
});
