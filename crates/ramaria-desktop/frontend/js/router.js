/**
 * js/router.js — Ramaria 视图路由系统
 *
 * 职责:
 * - 基于 Rust AppState 状态机事件驱动路由，将状态映射到对应视图
 * - 管理视图切换生命周期（enter / leave 钩子）
 * - 管理全屏视图的 Sidebar/Header/StatusBar 显隐逻辑
 * - 管理 Degraded 警告条
 * - 管理 Sidebar 导航激活态同步
 * - 管理状态栏指示器
 *
 * 设计特点:
 * - 订阅 Store.appState 变化自动路由，不依赖轮询
 * - 同时监听 Tauri 'app-state-changed' 事件作为后端推送更新
 * - 视图切换生命周期：leave(旧视图) → DOM 切换 → enter(新视图)
 * - 全屏视图（setup/progress/error）自动隐藏 Sidebar/Header/StatusBar
 * - 所有 DOM 操作集中在此模块，Store 和 Api 不操作 DOM
 * - 调试日志使用 [Router] 前缀
 *
 * 状态 → 视图映射（值来自 Rust AppState::as_str，均为 snake_case）:
 * needs_setup → setup（全屏首次配置向导）
 * downloading_model → progress（全屏进度页）
 * indexing → progress（全屏进度页）
 * ready → chat（对话主界面，或上次用户选择的导航视图）
 * degraded → chat（对话界面 + 顶部警告条）
 * fatal_error → error（全屏错误页）
 *
 * 用法:
 * RamariaRouter.init; // 启动路由监听
 * RamariaRouter.destroy; // 销毁路由（清理订阅和事件监听）
 *
 * 依赖:
 * - RamariaStore（js/store.js，必须先加载）
 * - TauriBridge（js/tauri-bridge.js，必须先加载）
 * - 全局 CSS（layout.css 中 .view / .view--fullscreen / .degraded-banner 类）
 */

var RamariaRouter = (function () {
    'use strict';

 // =========================================================
 // 常量
 // =========================================================

 /** 视图名称与内容区标题映射 */
    var VIEW_TITLES = {
        chat: '对话',
        memory: '记忆',
        persona: '人格管理',
        import: '数据导入',
        settings: '设置',
        setup: '首次配置',
        progress: '处理中',
        error: '错误',
    };

 /** 全屏视图列表（这些视图出现时隐藏 Sidebar/Header/StatusBar） */
    var FULLSCREEN_VIEWS = ['setup', 'progress', 'error'];

 /** 状态指示灯颜色类映射 */
    var STATUS_DOT_CLASS = {
        ready: 'ready',
        degraded: 'degraded',
        fatal_error: 'error',
    };

 /** 状态文本映射 */
    var STATUS_TEXT = {
        needs_setup: '需要配置',
        downloading_model: '下载模型中',
        indexing: '索引构建中',
        ready: '就绪',
        degraded: '部分功能不可用',
        fatal_error: '严重错误',
    };

 // =========================================================
 // 内部状态
 // =========================================================

 /** 当前显示的视图名称 */
    var _currentView = null;

/** 上次在 Ready/Degraded 状态下用户选择的导航视图（用于状态恢复） */
    var _lastNavView = 'chat';

    /**
     * 最近一次 showView 调用时传入的 options。
     * 视图模块（chat/memory 等）可通过 getLastOptions() 获取。
     * 用于支持跨视图参数传递（如 sessionId、personaUid、fromView）。
     */
    var _lastOptions = {};

    /** 是否已初始化 */
    var _initialized = false;

 /** 取消订阅 Store 的函数 */
    var _unsubAppState = null;

 /** 取消 Tauri 事件监听的函数 */
    var _unlistenAppState = null;

 // =========================================================
 // DOM 引用缓存
 // =========================================================

 /** 快捷 DOM 查询 */
    function $(id) {
        return document.getElementById(id);
    }

    var _dom = {};

    function cacheDom() {
        _dom.app = $('app');
        _dom.sidebar = $('sidebar');
        _dom.contentTitle = $('content-title');
        _dom.contentActions = $('content-actions');
        _dom.degradedBanner = $('degraded-banner');
        _dom.degradedBannerText = $('degraded-banner-text');
        _dom.statusDot = $('status-indicator-dot');
        _dom.statusText = $('status-indicator-text');
        _dom.statusSession = $('status-session-info');
        _dom.progressTitle = $('progress-title');
        _dom.progressDesc = $('progress-desc');
        _dom.errorTitle = $('error-title');
        _dom.errorDetail = $('error-detail');
    }

 // =========================================================
 // 视图切换核心
 // =========================================================

 /**
 * 切换到指定视图。
 *
 * 参数:
 * - `viewName`: 视图名称（'chat' | 'memory' | 'persona' | 'import' | 'settings' | 'setup' | 'progress' | 'error'）
 * - `options`: 可选。{ title, subInfo } 用于进度页/错误页的自定义文案
 *
 * 说明:
 * - 如果目标视图与当前视图相同，跳过（避免重复触发 leave/enter）
 * - 先执行旧视图的 leave 钩子，再切换 DOM，最后执行新视图的 enter 钩子
 */
    function showView(viewName, options) {
        if (!viewName) {
            console.warn('[Router] showView 参数 viewName 为空');
            return;
        }

 // 允许强制重新进入（用于视图模块延迟加载后手动触发渲染）
        var forceReenter = options && options.forceReenter === true;

        if (_currentView === viewName && !forceReenter) {
            console.log('[Router] 视图未变化，跳过: ' + viewName);
            return;
        }

        options = options || {};

 // 保存 options 供视图 enter 钩子使用
        _lastOptions = options;

 // 1. 旧视图 leave 钩子
        if (_currentView) {
            _callViewHook(_currentView, 'leave');
        }

 // 2. 关闭所有视图
        var allViews = document.querySelectorAll('.view');
        for (var i = 0; i < allViews.length; i++) {
            allViews[i].classList.remove('active');
        }

 // 3. 激活目标视图
        var targetView = document.querySelector('.view[data-view="' + viewName + '"]');
        if (!targetView) {
            console.error('[Router] 未找到视图: ' + viewName);
            return;
        }
        targetView.classList.add('active');

 // 4. 全屏视图 → 隐藏 Sidebar/Header/StatusBar
        var isFullscreen = FULLSCREEN_VIEWS.indexOf(viewName) !== -1;
        if (_dom.app) {
            if (isFullscreen) {
                _dom.app.classList.add('has-fullscreen-view');
            } else {
                _dom.app.classList.remove('has-fullscreen-view');
            }
        }

 // 5. 更新标题
        if (_dom.contentTitle) {
            _dom.contentTitle.textContent = VIEW_TITLES[viewName] || viewName;
        }

 // 6. 更新 Sidebar 导航激活态（全屏视图不清除激活态，保留最后导航状态）
        if (!isFullscreen) {
            _updateSidebarActive(viewName);
            _lastNavView = viewName;
        }

 // 7. 视图特定文案
        _applyViewOptions(viewName, options);

 // 8. 新视图 enter 钩子
        _currentView = viewName;
        _callViewHook(viewName, 'enter');

 // 9. 同步到 Store（静默，避免循环触发 Router）
        RamariaStore.set('currentView', viewName, true);

        console.log('[Router] 视图切换: ' + viewName + (isFullscreen ? ' (全屏)' : ''));
    }

 /**
 * 更新 Sidebar 导航链接的激活态。
 */
    function _updateSidebarActive(viewName) {
        var allNavLinks = document.querySelectorAll('.sidebar-nav-link[data-view]');
        for (var i = 0; i < allNavLinks.length; i++) {
            allNavLinks[i].classList.remove('active');
            allNavLinks[i].removeAttribute('aria-current');
        }

        var activeNav = document.querySelector('.sidebar-nav-link[data-view="' + viewName + '"]');
        if (activeNav) {
            activeNav.classList.add('active');
            activeNav.setAttribute('aria-current', 'page');
        }
    }

 /**
 * 对进度页和错误页应用自定义文案。
 */
    function _applyViewOptions(viewName, options) {
        if (viewName === 'progress') {
            if (options.title && _dom.progressTitle) {
                _dom.progressTitle.textContent = options.title;
            }
            if (options.subInfo && _dom.progressDesc) {
                _dom.progressDesc.textContent = options.subInfo;
            }
        }

        if (viewName === 'error') {
            if (options.title && _dom.errorTitle) {
                _dom.errorTitle.textContent = options.title;
            }
            if (options.subInfo && _dom.errorDetail) {
                _dom.errorDetail.textContent = options.subInfo;
            }
        }
    }

 // =========================================================
 // 视图生命周期钩子
 // =========================================================

 /**
 * 视图生命周期钩子注册表。
 * 格式: { viewName: { enter: [fn, ...], leave: [fn, ...] } }
 * 后续 Batch 5 的视图模块会通过 registerHook 注册自己的 enter/leave 逻辑。
 */
    var _hooks = {};

 /**
 * 注册视图生命周期钩子。
 *
 * 参数:
 * - `viewName`: 视图名称
 * - `phase`: 'enter' | 'leave'
 * - `fn`: 回调函数，接收 (viewName, options) 参数
 *
 * 返回:
 * - 取消注册的函数
 */
    function registerHook(viewName, phase, fn) {
        if (!_hooks[viewName]) {
            _hooks[viewName] = { enter: [], leave: [] };
        }
        _hooks[viewName][phase].push(fn);

        return function unregister() {
            var list = _hooks[viewName] && _hooks[viewName][phase];
            if (!list) return;
            var idx = list.indexOf(fn);
            if (idx !== -1) list.splice(idx, 1);
        };
    }

 /**
 * 调用视图的 enter/leave 钩子。
 * 每个钩子被 try-catch 包裹，单个失败不影响其他钩子。
 */
    function _callViewHook(viewName, phase) {
        var hooks = _hooks[viewName] && _hooks[viewName][phase];
        if (!hooks || hooks.length === 0) return;

        // enter 钩子接收 _lastOptions 作为第二个参数
        // leave 钩子不传 options（旧视图不应感知新视图的参数）
        var hookOptions = (phase === 'enter') ? _lastOptions : undefined;

        for (var i = 0; i < hooks.length; i++) {
            try {
                hooks[i](viewName, hookOptions);
            } catch (err) {
                console.error('[Router] 视图钩子异常 (' + viewName + '.' + phase + '):', err);
            }
        }
    }

 // =========================================================
 // AppState → 视图路由
 // =========================================================

 /**
 * 根据应用状态决定显示哪个视图。
 * 订阅 Store.appState 变化时自动调用。
 */
    function _routeByAppState(appState, oldState) {
        if (!appState) {
            console.warn('[Router] 收到空状态');
            return;
        }

        console.log('[Router] 状态变更: ' + (oldState || '初始') + ' → ' + appState);

 // 更新状态栏指示灯
        _updateStatusIndicator(appState);

 // 清除 Degraded 警告条（每次状态变更时重置）
        _hideDegradedBanner();

        switch (appState) {
            case 'needs_setup':
                showView('setup', {
                    title: '欢迎使用 Ramaria',
                    subInfo: '请完成首次配置以开始使用',
                });
                break;

            case 'downloading_model':
                showView('progress', {
                    title: '正在下载 Embedding 模型',
                    subInfo: '首次启动需要下载模型文件，请耐心等待...',
                });
                break;

            case 'indexing':
                showView('progress', {
                    title: '正在重建记忆索引',
                    subInfo: '正在处理历史数据，完成后将自动进入对话界面。',
                });
                break;

            case 'ready':
 // 从全屏视图（setup/progress）恢复时，回到上次的导航视图
                var wasFullscreen = _currentView && FULLSCREEN_VIEWS.indexOf(_currentView) !== -1;
                var targetView = wasFullscreen ? _lastNavView : (_currentView || 'chat');
 // 确保目标视图不是全屏视图
                if (FULLSCREEN_VIEWS.indexOf(targetView) !== -1) {
                    targetView = 'chat';
                }
                showView(targetView);
                break;

            case 'degraded':
 // 显示对话界面 + 警告条
                showView('chat');
 // 根据 Store 中的 degradedReason 显示具体原因
                var reason = RamariaStore.get('degradedReason') || '';
                var msg;
                if (reason === 'embedding_missing') {
                    msg = '⚠ 嵌入模型未配置 — 向量检索不可用，仅 BM25 + 图谱通道可用。请在「设置 → 嵌入模型」中配置或通过首次配置向导完成。';
                } else if (reason === 'llm_unavailable') {
                    msg = '⚠ LLM 后端暂不可用 — 请检查 LLM 后端服务连接后重试';
                } else if (reason === 'both_unavailable') {
                    msg = '⚠ LLM 后端与嵌入模型均不可用 — 请检查配置后重试';
                } else {
                    msg = '⚠ 部分功能不可用 — 请检查「设置」中各配置项状态';
                }
                _showDegradedBanner(msg);
                break;

            case 'fatal_error':
                showView('error', {
                    title: '严重错误',
                    subInfo: '应用遇到不可恢复的错误，请查看日志后重启。',
                });
                break;

            default:
                console.warn('[Router] 未知应用状态: ' + appState);
 // 保守处理：显示错误页
                showView('error', {
                    title: '未知状态',
                    subInfo: '应用状态异常 (' + appState + ')，请重启应用。',
                });
                break;
        }
    }

 // =========================================================
 // 状态栏与警告条
 // =========================================================

 /**
 * 更新状态栏指示灯和文本。
 */
    function _updateStatusIndicator(appState) {
        if (_dom.statusDot) {
            var dotClass = STATUS_DOT_CLASS[appState] || '';
            _dom.statusDot.className = 'status-bar-dot ' + dotClass;
        }
        if (_dom.statusText) {
            _dom.statusText.textContent = STATUS_TEXT[appState] || appState;
        }
    }

 /**
 * 显示 Degraded 警告条。
 */
    function _showDegradedBanner(text) {
        if (_dom.degradedBanner) {
            _dom.degradedBanner.classList.remove('hidden');
        }
        if (_dom.degradedBannerText) {
            _dom.degradedBannerText.textContent = text || '部分功能不可用';
        }
    }

 /**
 * 隐藏 Degraded 警告条。
 */
    function _hideDegradedBanner() {
        if (_dom.degradedBanner) {
            _dom.degradedBanner.classList.add('hidden');
        }
    }

 // =========================================================
 // Sidebar 导航事件
 // =========================================================

 /**
 * 初始化 Sidebar 导航链接的点击事件。
 * 用户点击导航时直接切视图（不依赖 AppState 变更）。
 */
    function _initSidebarNavigation() {
        var navLinks = document.querySelectorAll('.sidebar-nav-link[data-view]');
        for (var i = 0; i < navLinks.length; i++) {
            navLinks[i].addEventListener('click', function () {
                var viewName = this.getAttribute('data-view');
                if (viewName) {
 // 全屏视图下禁止 Sidebar 导航
                    if (_currentView && FULLSCREEN_VIEWS.indexOf(_currentView) !== -1) {
                        console.log('[Router] 全屏视图下忽略 Sidebar 导航: ' + viewName);
                        return;
                    }
                    showView(viewName);
                }
            });
        }
    }

 // =========================================================
 // 更新状态栏会话信息
 // =========================================================

 /**
 * 更新状态栏的会话信息显示。
 * 由 Store.activeSessionId 变化时触发。
 */
    function _updateSessionInfo(sessionId) {
        if (_dom.statusSession) {
            if (sessionId) {
 // 显示会话 ID 短格式（前 8 位）
                _dom.statusSession.textContent = '会话: ' + sessionId.substring(0, 8) + '...';
            } else {
                _dom.statusSession.textContent = '';
            }
        }
    }

 // =========================================================
 // 初始化与销毁
 // =========================================================

 /**
 * 初始化路由系统。
 *
 * 说明:
 * - 缓存 DOM 引用
 * - 绑定 Sidebar 导航事件
 * - 订阅 Store.appState 变化
 * - 监听 Tauri app-state-changed 事件
 * - 订阅 Store.activeSessionId 以更新状态栏
 */
    function init() {
        if (_initialized) {
            console.warn('[Router] 已初始化，跳过重复 init');
            return;
        }

        console.log('[Router] 初始化路由系统...');

 // 1. 缓存 DOM 引用
        cacheDom();

 // 2. 绑定 Sidebar 导航
        _initSidebarNavigation();

 // 3. 订阅 Store.appState 变化
        _unsubAppState = RamariaStore.subscribe('appState', _routeByAppState);

 // 4. 订阅 Store.activeSessionId 变化 → 更新状态栏
        RamariaStore.subscribe('activeSessionId', _updateSessionInfo);

 // 5. 监听 Tauri app-state-changed 事件（Rust 后端推送）
        try {
            if (TauriBridge && TauriBridge.isTauri && TauriBridge.isTauri()) {
                TauriBridge.listen('app-state-changed', function (event) {
                    var payload = event.payload;
                    if (payload && payload.state) {
                        console.log('[Router] 收到后端状态推送: ' + payload.state);
 // 同步到 Store，触发 Store 的 'appState' 通知 → _routeByAppState
                        RamariaStore.set('appState', payload.state);
                    }
                }).then(function (unlisten) {
                    _unlistenAppState = unlisten;
                    console.log('[Router] Tauri app-state-changed 监听已注册');
                }).catch(function (err) {
                    console.error('[Router] 无法监听 app-state-changed 事件:', err);
                });
            }
        } catch (err) {
            console.warn('[Router] Tauri 事件监听设置失败（可能非 Tauri 环境）:', err);
        }

 // 6. 执行初始路由（基于 Store 中当前的 appState）
 // 必须主动触发，因为 app.js 后续 set 可能因值相等(===)被跳过
        var currentState = RamariaStore.get('appState');
        if (currentState) {
            console.log('[Router] 执行初始路由: ' + currentState);
            _routeByAppState(currentState, null);
        }

        _initialized = true;
        console.log('[Router] 路由系统初始化完成');
    }

 /**
 * 销毁路由系统。
 * 取消所有订阅和事件监听，清理钩子注册表。
 */
    function destroy() {
        console.log('[Router] 销毁路由系统...');

 // 取消 Store 订阅
        if (_unsubAppState) {
            _unsubAppState();
            _unsubAppState = null;
        }

 // 取消 Tauri 事件监听
        if (_unlistenAppState) {
            try { _unlistenAppState(); } catch (_) { /* ignore */ }
            _unlistenAppState = null;
        }

 // 清理钩子注册表
        _hooks = {};

        _currentView = null;
        _initialized = false;
        console.log('[Router] 路由系统已销毁');
    }

 // =========================================================
 // 公开 API
 // =========================================================

    return {
        /** 初始化路由系统 */
        init: init,
        /** 销毁路由系统 */
        destroy: destroy,

        /** 切换到指定视图（外部调用，如 Toast 后导航） */
        showView: showView,

        /** 获取当前视图名称 */
        getCurrentView: function () { return _currentView; },

        /** 获取上次导航视图 */
        getLastNavView: function () { return _lastNavView; },

        /**
         * 获取最近一次 showView 调用传入的 options。
         * 视图模块用于读取跨视图传递的参数（如 sessionId、personaUid、fromView）。
         *
         * 返回:
         * - 最近一次 showView 的 options 对象（可能为空 {}）
         */
        getLastOptions: function () { return _lastOptions; },

 /** 注册视图生命周期钩子 */
        registerHook: registerHook,

 /** 更新状态栏文本（由视图模块调用，如显示 session 名称） */
        setStatusText: function (text) {
            if (_dom.statusText) _dom.statusText.textContent = text;
        },

 /** 更新状态栏会话信息 */
        setSessionInfo: function (text) {
            if (_dom.statusSession) _dom.statusSession.textContent = text;
        },

 /** 更新内容区标题（由视图模块调用） */
        setContentTitle: function (title) {
            if (_dom.contentTitle) _dom.contentTitle.textContent = title;
        },

 /** 更新内容区操作按钮区（由内部视图模块注入，调用方保证 HTML 可信） */
        setContentActions: function (html) {
            if (_dom.contentActions) _dom.contentActions.innerHTML = html || '';
        },

 /** 是否已初始化 */
        isInitialized: function () { return _initialized; },
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaRouter', {
    value: RamariaRouter,
    writable: false,
    configurable: false,
});
