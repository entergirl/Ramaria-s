/**
 * js/app.js — Ramaria 桌面应用入口
 *
 * 职责:
 * - 应用启动初始化：按序加载 Store → Router → 查询 AppState → 首次路由
 * - 管理暗/亮主题切换（data-theme + localStorage 持久化）
 * - 处理非 Tauri 环境的降级展示
 * - 为后续 Batch 5 视图模块提供全局挂载点
 *
 * 设计特点:
 * - 不直接管理视图切换（由 Router 负责）
 * - 不直接管理状态（由 Store 负责）
 * - 不直接调用 TauriBridge（由 Api 负责）
 * - 仅做编排：初始化 → 查询初始状态 → 触发首次路由 → 监听后续变更
 * - 保留 RamariaApp 全局命名空间供外部调试和后续扩展
 *
 * 初始化流程:
 * 1. cacheDom → 恢复主题
 * 2. Router.init → 订阅 Store + Tauri 事件
 * 3. 绑定主题切换按钮
 * 4. 查询 get_app_state → Store.set('appState', ...) → Router 自动路由
 * 5. 非 Tauri 环境降级展示
 *
 * 依赖:
 * - TauriBridge（js/tauri-bridge.js）
 * - RamariaStore（js/store.js）
 * - RamariaApi（js/api.js）
 * - RamariaRouter（js/router.js）
 * - CSS: tokens.css / reset.css / layout.css / utilities.css
 */
(function () {
    'use strict';

 // =========================================================
 // 常量
 // =========================================================

 /**
  * 应用版本号。
  *
  * 初始为占位（''），启动时经 `get_version` 后端命令（编译期
  * `env!("CARGO_PKG_VERSION")`）获取真实版本并写入全局变量
  * `window.RamariaApp.version`；浏览器预览模式（Tauri IPC 不可用）
  * 时保持占位，不阻塞启动。
  */
    var APP_VERSION = '';

 /** 开发模式标志（通过 URL 参数 ?dev 启用） */
    var IS_DEV = (function () {
        try {
            return window.location.search.indexOf('dev') !== -1;
        } catch (_) { return false; }
    })();

 // =========================================================
 // 环境检测
 // =========================================================

    var isTauri = false;
    try {
        isTauri = !!(TauriBridge && TauriBridge.isTauri && TauriBridge.isTauri());
    } catch (_) {
 /* TauriBridge 可能未加载 */
    }

    if (!isTauri) {
        console.warn('[App] 未检测到 Tauri 环境。将以浏览器预览模式运行。');
    }

    if (IS_DEV) {
        console.log('[App] 开发模式已启用（?dev），将输出更多调试信息');
    }

 // =========================================================
 // DOM 引用缓存
 // =========================================================

    var $ = function (id) {
        return document.getElementById(id);
    };

    var dom = {
        app: $('app'),
        contentTitle: $('content-title'),
        statusText: $('status-indicator-text'),
        statusSession: $('status-session-info'),
        btnToggleTheme: $('btn-toggle-theme'),
    };

 // =========================================================
 // 主题管理
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
 */
    function toggleTheme() {
        var current = getCurrentTheme();
        var next = current === 'dark' ? 'light' : 'dark';
        setTheme(next);
    }

 /**
 * 设置主题。
 *
 * 参数:
 * - `theme`: 'dark' | 'light'
 */
    function setTheme(theme) {
        if (theme !== 'dark' && theme !== 'light') {
            console.warn('[App] 无效主题: ' + theme + '，使用 light');
            theme = 'light';
        }

        document.documentElement.setAttribute('data-theme', theme);

 // 深色模式不持久化，每次启动默认浅色
 // 不写入 localStorage

 // 更新主题按钮图标
        if (dom.btnToggleTheme) {
            var iconEl = dom.btnToggleTheme.querySelector('.sidebar-nav-icon');
            if (iconEl) {
                iconEl.textContent = theme === 'dark' ? '🌙' : '☀️';
            }
        }

        console.log('[App] 主题切换: ' + theme);
    }

 /**
 * 初始化主题为浅色（不再从 localStorage 恢复）。
 */
    function restoreTheme() {
 // 始终默认浅色，不读取 localStorage
        setTheme('light');
    }

 // =========================================================
 // 非 Tauri 环境降级
 // =========================================================

 /**
 * 非 Tauri 环境时显示浏览器预览模式提示。
 */
    function showBrowserFallback() {
        console.log('[App] 浏览器预览模式：Tauri IPC 不可用');

        if (dom.statusText) {
            dom.statusText.textContent = '浏览器预览模式';
        }

 // 通过 Router 触发路由到对话视图（Router 在 init 流程中已先初始化）
        if (RamariaRouter && RamariaRouter.isInitialized && RamariaRouter.isInitialized()) {
            RamariaStore.set('appState', 'ready');
        } else {
 // 极端情况：Router 未初始化，直接操作 DOM
            _fallbackShowChat();
        }
    }

 /**
 * 降级显示对话视图（不依赖 Router）。
 */
    function _fallbackShowChat() {
        var chatView = document.querySelector('.view[data-view="chat"]');
        if (chatView) {
            chatView.classList.add('active');
        }
        if (dom.contentTitle) {
            dom.contentTitle.textContent = '对话（预览）';
        }
    }

 // =========================================================
 // 初始化
 // =========================================================

 /**
 * 从后端获取真实版本号并路由到全局变量与 DOM。
 *
 * 行为:
 * - 调用 RamariaApi.diagnostics.getVersion()（后端 `get_version` →
 *   编译期 `env!("CARGO_PKG_VERSION")`，与 Cargo.toml 单一来源同步）。
 * - 写入全局变量 `window.RamariaApp.version`（延迟到 RamariaApp 定义后
 *   由 `Object.defineProperty` 保护——此处通过对象属性赋值更新）。
 * - 更新主页面左上角品牌区 `#sidebar-brand-version`。
 * - 失败静默降级：保持占位，不阻塞启动（浏览器预览模式同理）。
 */
    async function loadAppVersion() {
        try {
            var version = await RamariaApi.diagnostics.getVersion();
            if (!version) return;
            APP_VERSION = version;

 // 更新全局变量（RamariaApp 对象属性可写，仅对象引用被 defineProperty 冻结）
            if (window.RamariaApp) {
                window.RamariaApp.version = version;
            }

 // 更新左上角品牌区版本号
            var brandVersionEl = $('sidebar-brand-version');
            if (brandVersionEl) {
                brandVersionEl.textContent = 'v' + version;
            }
        } catch (_) {
 // 静默忽略加载失败（预览模式 / IPC 异常）
        }
    }

 /**
 * 主初始化流程。
 *
 * 执行顺序:
 * 0. 加载真实版本号（get_version → 全局变量 → 左上角品牌区）
 * 1. 恢复主题偏好
 * 2. 绑定主题切换按钮
 * 3. 初始化 Router（注册 Store 订阅 + Tauri 事件）
 * 4. Tauri 环境：查询 get_app_state → 触发首次路由
 * 5. 非 Tauri 环境：降级展示
 */
    async function init() {
 // 0. 加载真实版本号（Tauri 环境）；浏览器预览模式静默跳过
        if (isTauri) {
            await loadAppVersion();
        }

        console.log('[App] Ramaria v' + (APP_VERSION || '?') + ' 桌面应用启动中...');
        console.log('[App] 环境: ' + (isTauri ? 'Tauri WebView' : '浏览器预览'));

 // 1. 恢复主题
        restoreTheme();

 // 2. 绑定主题切换按钮
        if (dom.btnToggleTheme) {
            dom.btnToggleTheme.addEventListener('click', toggleTheme);
        }

 // 3. 初始化 Router（Router 内部会订阅 Store + Tauri 事件）
        try {
            RamariaRouter.init();
            console.log('[App] Router 已初始化');
        } catch (err) {
            console.error('[App] Router 初始化失败:', err);
        }

 // 4. 查询初始应用状态
        if (isTauri) {
            try {
                console.log('[App] 正在查询应用状态...');
                var state = await RamariaApi.chat.getAppState();
                console.log('[App] 初始状态: ' + state);

 // 如果是 degraded，先查询具体原因再设置状态
 // 必须在 set('appState') 之前完成，否则 Router 响应 appState 变化时
 // degradedReason 还是 null，导致永远走默认提示分支
                if (state === 'degraded') {
                    try {
                        var reason = await RamariaApi.setup.getDegradedReason();
                        RamariaStore.set('degradedReason', reason || '');
                    } catch (_) {
                        RamariaStore.set('degradedReason', '');
                    }
                }

 // 同步到 Store，Router 订阅的 'appState' 事件会自动触发首次路由
                RamariaStore.set('appState', state);

 // 如果状态是 ready/degraded，预加载会话列表和配置
                if (state === 'ready' || state === 'degraded') {
                    _preloadData();
                }

 // ★ 监听关闭窗口确认事件
 // Rust 端拦截 CloseRequested → 发送 close-requested 事件 →
 // 前端弹窗让用户选择「最小化到托盘」或「退出 Ramaria」
                _listenCloseRequested();
            } catch (err) {
                console.error('[App] 无法获取应用状态:', err);
 // 无法连接后端 → 显示错误
                RamariaStore.set('appState', 'fatal_error', true);
                RamariaRouter.showView('error', {
                    title: '无法连接后端',
                    subInfo: '请确认应用已正确启动。错误详情：' + (err.message || String(err)),
                });
                if (dom.statusText) {
                    dom.statusText.textContent = '连接失败';
                }
            }
        } else {
 // 非 Tauri 环境降级
            showBrowserFallback();
        }

 // 5. 开发模式下暴露调试接口
        if (IS_DEV) {
            window.__RAMARIA_DEV__ = {
                store: RamariaStore,
                api: RamariaApi,
                router: RamariaRouter,
                version: APP_VERSION,
                isTauri: isTauri,
            };
            console.log('[App] 开发调试接口已挂载到 window.__RAMARIA_DEV__');
        }

        console.log('[App] 初始化完成');
    }

 /**
 * 预加载应用数据（会话列表、配置、人格列表、全局设置）。
 * 并行发起 4 个请求，各自独立处理成功/失败，不阻塞主流程。
 */
    function _preloadData() {
        console.log('[App] 预加载应用数据...');

 // 并行发起，各自 .catch 保证不抛异常
        RamariaApi.session.list().then(function (sessions) {
            RamariaStore.set('sessions', sessions);
            console.log('[App] 会话列表已加载 (' + sessions.length + ' 个)');
        }).catch(function (err) {
            console.warn('[App] 加载会话列表失败:', err.message || err);
        });

        RamariaApi.config.getBackend().then(function (config) {
            RamariaStore.set('backendConfig', config);
            console.log('[App] 后端配置已加载 (' + config.provider + ')');
        }).catch(function (err) {
            console.warn('[App] 加载后端配置失败:', err.message || err);
        });

        RamariaApi.memory.getPersonas().then(function (personas) {
            RamariaStore.set('personas', personas);
            console.log('[App] 人格列表已加载 (' + personas.length + ' 个)');
        }).catch(function (err) {
            console.warn('[App] 加载人格列表失败:', err.message || err);
        });

        RamariaApi.config.getSettings().then(function (settings) {
            RamariaStore.set('settings', settings);
            console.log('[App] 全局设置已加载 (' + settings.length + ' 项)');
        }).catch(function (err) {
            console.warn('[App] 加载全局设置失败:', err.message || err);
        });
    }

 /**
 * 监听 Rust 端 close-requested 事件，弹窗让用户确认关闭操作。
 *
 * 弹窗选项:
 * - 「最小化到托盘」：调用 confirm_close_action("minimize") → 窗口隐藏
 * - 「退出 Ramaria」：调用 confirm_close_action("exit") → 应用退出
 *
 * 说明:
 * - 事件由 tray.rs 的 intercept_close_event 在用户点击 × 按钮时发送
 * - 弹窗不可关闭（无 × 按钮、ESC 不生效、遮罩点击不关闭），
 * 强制用户二选一，防止误操作关闭窗口
 */
    function _listenCloseRequested() {
        if (!TauriBridge || !TauriBridge.isTauri || !TauriBridge.isTauri()) {
            return; // 非 Tauri 环境，不需要监听
        }

        TauriBridge.listen('close-requested', function () {
            console.log('[App] 收到关闭请求，显示确认弹窗');

            RamariaModal.show({
                title: '关闭 Ramaria',
                body: '<p class="app-close-body">请选择关闭方式：</p>' +
                    '<div class="app-close-detail">' +
                    '• <strong>最小化到托盘</strong>：窗口隐藏，应用在后台继续运行。<br>' +
                    '   可通过系统托盘图标恢复窗口。<br>' +
                    '• <strong>退出 Ramaria</strong>：完全关闭应用，停止所有后台任务。' +
                    '</div>',
                footer:
                    '<button class="btn btn-secondary flex-1" data-action="minimize">最小化到托盘</button>' +
                    '<button class="btn btn-primary flex-1" data-action="exit">退出 Ramaria</button>',
                closable: false,        // 禁止 × 关闭
                closeOnOverlay: false,  // 禁止点击遮罩关闭
                closeOnEsc: false,      // 禁止 ESC 关闭
                size: 'sm',
                onAction: function (action) {
                    console.log('[App] 用户选择关闭操作: ' + action);
 // 根据用户选择调用后端命令
                    TauriBridge.invoke('confirm_close_action', { action: action })
                        .then(function () {
 // "minimize" 后前端暂停渲染，"exit" 后应用已退出
                            console.log('[App] 关闭操作已执行: ' + action);
                        })
                        .catch(function (err) {
                            console.error('[App] 关闭操作失败:', err);
 // 操作失败时关闭弹窗，恢复窗口正常状态
                            RamariaModal.close();
                        });
                },
            });
        }).catch(function (err) {
            console.error('[App] close-requested 事件监听注册失败:', err);
        });
    }

 // =========================================================
 // 公开 API（保持 Batch 2 的 RamariaApp 接口兼容性）
 // =========================================================

    window.RamariaApp = {
 /** 版本号 */
        version: APP_VERSION,

 /** 是否在 Tauri 环境中 */
        isTauri: function () { return isTauri; },

 /** 获取当前主题 */
        getCurrentTheme: getCurrentTheme,

 /** 设置主题 */
        setTheme: setTheme,

 /** 切换主题 */
        toggleTheme: toggleTheme,

 /** 获取当前视图（委托给 Router） */
        getCurrentView: function () {
            return RamariaRouter ? RamariaRouter.getCurrentView() : null;
        },

 /** 切换到指定视图（委托给 Router） */
        showView: function (viewName, options) {
            if (RamariaRouter) {
                RamariaRouter.showView(viewName, options);
            }
        },

 /** Store 引用（调试用） */
        getStore: function () { return RamariaStore; },

 /** Api 引用（调试用） */
        getApi: function () { return RamariaApi; },

 /** Router 引用（调试用） */
        getRouter: function () { return RamariaRouter; },
    };

 // 防止意外覆盖
    Object.defineProperty(window, 'RamariaApp', {
        value: window.RamariaApp,
        writable: false,
        configurable: false,
    });

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
