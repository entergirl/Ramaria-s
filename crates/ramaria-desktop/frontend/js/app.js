/**
 * js/app.js — Ramaria 桌面应用入口
 *
 * 职责:
 * - 检测 Tauri 环境可用性
 * - 查询当前 AppState 并路由到对应视图
 * - 管理暗/亮主题切换（data-theme 属性）
 * - 监听 app-state-changed 事件自动更新 UI 状态
 * - Sidebar 导航切换（对话/记忆/设置）
 * - 为后续 Batch 3 Router 提供基础钩子
 *
 * 安全:
 * - 不在 console 输出任何敏感信息（API key、用户消息）
 * - 不在 DOM 中渲染未 sanitize 的 HTML（后续 markdown.js 负责）
 *
 * 依赖:
 * - TauriBridge（js/tauri-bridge.js，必须先于本文件加载）
 * - 设计系统 CSS（tokens.css / reset.css / layout.css / utilities.css）
 */

(function () {
    'use strict';

    // =========================================================
    // 常量
    // =========================================================

    /** 视图名称与标题映射 */
    var VIEW_TITLES = {
        chat: '对话',
        memory: '记忆',
        settings: '设置',
        setup: '首次配置',
        progress: '处理中',
        error: '错误',
    };

    /** 需要隐藏 Sidebar 的视图（全屏视图） */
    var FULLSCREEN_VIEWS = ['setup', 'progress', 'error'];

    /** 状态指示灯颜色类 */
    var STATUS_DOT_CLASS = {
        Ready: 'ready',
        Degraded: 'degraded',
        FatalError: 'error',
    };

    // =========================================================
    // 环境检测
    // =========================================================

    var isTauri = false;
    try {
        isTauri = TauriBridge && TauriBridge.isTauri && TauriBridge.isTauri();
    } catch (_) {
        /* TauriBridge 可能未加载 */
    }

    if (!isTauri) {
        console.warn('[App] 未检测到 Tauri 环境。部分功能不可用。');
    }

    // =========================================================
    // DOM 引用缓存
    // =========================================================

    var $ = function (id) {
        return document.getElementById(id);
    };

    var dom = {
        app: $('app'),
        sidebar: $('sidebar'),
        contentTitle: $('content-title'),
        contentActions: $('content-actions'),
        degradedBanner: $('degraded-banner'),
        degradedBannerText: $('degraded-banner-text'),
        statusDot: $('status-indicator-dot'),
        statusText: $('status-indicator-text'),
        statusSession: $('status-session-info'),
        progressTitle: $('progress-title'),
        progressDesc: $('progress-desc'),
        errorTitle: $('error-title'),
        errorDetail: $('error-detail'),
        btnToggleTheme: $('btn-toggle-theme'),
    };

    // =========================================================
    // 视图管理
    // =========================================================

    /** 当前显示的视图名称 */
    var currentView = null;

    /**
     * 切换到指定视图。
     *
     * 参数:
     * - `viewName`: 视图名称（'chat' | 'memory' | 'settings' | 'setup' | 'progress' | 'error'）
     * - `options`: 可选。{ title, subInfo } 用于进度页/错误页
     */
    function showView(viewName, options) {
        if (!viewName) return;

        options = options || {};

        // 1. 关闭所有视图
        var allViews = document.querySelectorAll('.view');
        for (var i = 0; i < allViews.length; i++) {
            allViews[i].classList.remove('active');
        }

        // 2. 激活目标视图
        var targetView = document.querySelector('.view[data-view="' + viewName + '"]');
        if (!targetView) {
            console.error('[App] 未找到视图: ' + viewName);
            return;
        }
        targetView.classList.add('active');

        // 3. 处理全屏视图
        var isFullscreen = FULLSCREEN_VIEWS.indexOf(viewName) !== -1;
        if (isFullscreen) {
            dom.app.classList.add('has-fullscreen-view');
        } else {
            dom.app.classList.remove('has-fullscreen-view');
        }

        // 4. 更新标题
        dom.contentTitle.textContent = VIEW_TITLES[viewName] || viewName;

        // 5. 更新 Sidebar 激活态
        var allNavLinks = document.querySelectorAll('.sidebar-nav-link[data-view]');
        for (var j = 0; j < allNavLinks.length; j++) {
            allNavLinks[j].classList.remove('active');
            allNavLinks[j].removeAttribute('aria-current');
        }

        if (!isFullscreen) {
            var activeNav = document.querySelector('.sidebar-nav-link[data-view="' + viewName + '"]');
            if (activeNav) {
                activeNav.classList.add('active');
                activeNav.setAttribute('aria-current', 'page');
            }
        }

        // 6. 进度页特殊处理
        if (viewName === 'progress') {
            if (dom.progressTitle) dom.progressTitle.textContent = options.title || '正在处理...';
            if (dom.progressDesc) dom.progressDesc.textContent = options.subInfo || '请稍候';
        }

        // 7. 错误页特殊处理
        if (viewName === 'error') {
            if (dom.errorTitle) dom.errorTitle.textContent = options.title || '严重错误';
            if (dom.errorDetail) dom.errorDetail.textContent = options.subInfo || '应用遇到不可恢复的错误，请查看日志后重启。';
        }

        currentView = viewName;
        console.log('[App] 视图切换: ' + viewName);
    }

    // =========================================================
    // 状态指示与 UI 更新
    // =========================================================

    /**
     * 根据 Rust AppState 更新 UI 状态指示。
     *
     * 状态映射（对齐 ramaria-core AppState 枚举）:
     * - NeedsSetup       → 首次配置向导（全屏）
     * - DownloadingModel → 进度页（全屏）
     * - Indexing         → 进度页（全屏）
     * - Ready            → 对话页（默认）
     * - Degraded         → 对话页 + 顶部警告条
     * - FatalError       → 错误页（全屏）
     */
    function updateAppState(state) {
        if (!state) return;

        // 清除 Degraded 警告条
        dom.degradedBanner.classList.add('hidden');

        // 更新状态指示灯
        var dotClass = STATUS_DOT_CLASS[state] || '';
        dom.statusDot.className = 'status-bar-dot ' + dotClass;

        switch (state) {
            case 'NeedsSetup':
                dom.statusText.textContent = '需要配置';
                showView('setup');
                break;

            case 'DownloadingModel':
                dom.statusText.textContent = '下载模型中';
                showView('progress', {
                    title: '正在下载 Embedding 模型',
                    subInfo: '首次启动需要下载模型文件，请耐心等待...',
                });
                break;

            case 'Indexing':
                dom.statusText.textContent = '索引构建中';
                showView('progress', {
                    title: '正在重建记忆索引',
                    subInfo: '正在处理历史数据，完成后将自动进入对话界面。',
                });
                break;

            case 'Ready':
                dom.statusText.textContent = '就绪';
                // 如果当前是全屏视图，切回对话页
                if (!currentView || FULLSCREEN_VIEWS.indexOf(currentView) !== -1) {
                    showView('chat');
                }
                break;

            case 'Degraded':
                dom.statusText.textContent = '部分功能不可用';
                dom.degradedBanner.classList.remove('hidden');
                dom.degradedBannerText.textContent = '部分功能不可用 — 请检查 LLM 后端连接后重试';
                showView('chat');
                break;

            case 'FatalError':
                dom.statusText.textContent = '严重错误';
                showView('error', {
                    title: '严重错误',
                    subInfo: '应用遇到不可恢复的错误，请查看日志后重启。',
                });
                break;

            default:
                dom.statusText.textContent = state;
                console.warn('[App] 未知状态: ' + state);
                break;
        }

        console.log('[App] 状态更新: ' + state);
    }

    // =========================================================
    // 主题切换
    // =========================================================

    /**
     * 获取当前主题。
     * 返回: 'dark' | 'light'
     */
    function getCurrentTheme() {
        return document.documentElement.getAttribute('data-theme') || 'light';
    }

    /**
     * 切换暗/亮主题。
     * 同时更新 localStorage 和 UI 按钮图标。
     */
    function toggleTheme() {
        var current = getCurrentTheme();
        var next = current === 'dark' ? 'light' : 'dark';
        setTheme(next);
    }

    /**
     * 设置主题。
     * 参数: 'dark' | 'light'
     */
    function setTheme(theme) {
        document.documentElement.setAttribute('data-theme', theme);
        try {
            localStorage.setItem('ramaria-theme', theme);
        } catch (_) {
            /* localStorage 不可用（隐私模式等），静默降级 */
        }

        // 更新按钮图标
        if (dom.btnToggleTheme) {
            var iconEl = dom.btnToggleTheme.querySelector('.sidebar-nav-icon');
            if (iconEl) {
                iconEl.textContent = theme === 'dark' ? '🌙' : '☀️';
            }
        }

        console.log('[App] 主题切换: ' + theme);
    }

    /**
     * 从 localStorage 恢复主题偏好。
     */
    function restoreTheme() {
        try {
            var saved = localStorage.getItem('ramaria-theme');
            if (saved === 'light' || saved === 'dark') {
                setTheme(saved);
                return;
            }
        } catch (_) {
            /* 静默降级 */
        }
        // 默认浅色
        setTheme('light');
    }

    // =========================================================
    // Sidebar 导航事件
    // =========================================================

    function initSidebarNavigation() {
        var navLinks = document.querySelectorAll('.sidebar-nav-link[data-view]');
        for (var i = 0; i < navLinks.length; i++) {
            navLinks[i].addEventListener('click', function () {
                var viewName = this.getAttribute('data-view');
                if (viewName) {
                    showView(viewName);
                }
            });
        }
    }

    // =========================================================
    // Tauri 事件监听
    // =========================================================

    function initTauriEvents() {
        if (!isTauri) return;

        // 监听 Rust 推送的 app-state-changed 事件
        TauriBridge.listen('app-state-changed', function (event) {
            var payload = event.payload;
            if (payload && payload.state) {
                updateAppState(payload.state);
            }
        }).catch(function (err) {
            console.error('[App] 无法监听 app-state-changed 事件:', err);
        });
    }

    // =========================================================
    // 初始化
    // =========================================================

    async function init() {
        console.log('[App] Ramaria 桌面应用启动中...');

        // 1. 恢复主题
        restoreTheme();

        // 2. 绑定主题切换按钮
        if (dom.btnToggleTheme) {
            dom.btnToggleTheme.addEventListener('click', toggleTheme);
        }

        // 3. 绑定 Sidebar 导航
        initSidebarNavigation();

        // 4. 绑定 Tauri 事件
        initTauriEvents();

        // 5. 查询应用状态并路由
        if (isTauri) {
            try {
                var state = await TauriBridge.invoke('get_app_state');
                updateAppState(state);
            } catch (err) {
                console.error('[App] 无法获取应用状态:', err);
                // 降级：显示错误页
                dom.statusText.textContent = '连接失败';
                showView('error', {
                    title: '无法连接后端',
                    subInfo: '请确认应用已正确启动。错误详情：' + (err.message || String(err)),
                });
            }
        } else {
            // 非 Tauri 环境（浏览器直接打开），显示占位页
            console.log('[App] 非 Tauri 环境，显示占位视图');
            dom.statusText.textContent = '浏览器预览模式';
            showView('chat');
        }

        console.log('[App] 初始化完成');
    }

    // =========================================================
    // 公开 API（供后续 Batch 3 Router 使用）
    // =========================================================

    window.RamariaApp = {
        /** 切换到指定视图 */
        showView: showView,
        /** 更新应用状态 */
        updateAppState: updateAppState,
        /** 获取当前视图 */
        getCurrentView: function () { return currentView; },
        /** 获取当前主题 */
        getCurrentTheme: getCurrentTheme,
        /** 设置主题 */
        setTheme: setTheme,
        /** 切换主题 */
        toggleTheme: toggleTheme,
        /** 是否在 Tauri 环境中 */
        isTauri: function () { return isTauri; },
    };

    // =========================================================
    // 启动
    // =========================================================

    // DOM 加载完成后启动
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }

})();
