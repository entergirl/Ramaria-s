/**
 * tauri-bridge.js — Ramaria Tauri 2 IPC 桥接层
 *
 * 设计特点:
 * - 基于 Tauri 2 原生 window.__TAURI__ API，无需 npm 依赖
 * - 提供 invoke() 和 listen() 两个核心方法，替代旧版 fetch/WebSocket
 * - 自动检测是否在 Tauri 环境中运行（非 Tauri 环境降级打印警告）
 * - 错误统一包装为 Error 对象，含友好中文提示
 *
 * 用法:
 *   const result = await TauriBridge.invoke('get_app_state');
 *   TauriBridge.listen('chat-delta', (event) => { console.log(event.payload); });
 *
 * 安全:
 * - 仅在 Tauri WebView 上下文中可用（window.__TAURI__ 由 Tauri 注入）
 * - 不暴露任何 window.__TAURI__ 内部方法到全局作用域
 *
 * Phase 5: 后续页面（chat/setup/memory/settings）均通过本桥接层与 Rust 通信
 */

const TauriBridge = (function () {
    'use strict';

    // =========================================================
    // 环境检测
    // =========================================================

    /** 是否在 Tauri WebView 环境中 */
    let _isTauri = false;

    try {
        _isTauri = !!(window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.event);
    } catch (_) {
        _isTauri = false;
    }

    if (!_isTauri) {
        console.warn('[TauriBridge] 未检测到 Tauri 环境，请在 Tauri 桌面应用中运行。');
    }

    // =========================================================
    // 核心 API
    // =========================================================

    /**
     * 调用 Tauri Rust 命令。
     *
     * 参数:
     * - `command`: 命令名称（如 "send_message"、"get_app_state"）
     * - `args`: 可选，命令参数对象
     *
     * 返回:
     * - Promise，成功时返回命令的返回值，失败时抛出 Error
     *
     * 示例:
     *   const state = await TauriBridge.invoke('get_app_state');
     *   const result = await TauriBridge.invoke('send_message', { message: '你好' });
     */
    async function invoke(command, args) {
        if (!_isTauri) {
            throw new Error('[TauriBridge] 不在 Tauri 环境中，无法调用命令: ' + command);
        }

        args = args || {};

        try {
            return await window.__TAURI__.core.invoke(command, args);
        } catch (err) {
            // 将 Tauri 内部错误包装为 Error，便于前端统一处理
            const message = typeof err === 'string' ? err : (err.message || String(err));
            console.error('[TauriBridge] 命令调用失败:', command, message);
            throw new Error(message);
        }
    }

    /**
     * 监听 Tauri 事件（替代 WebSocket）。
     *
     * 参数:
     * - `event`: 事件名称（如 "chat-delta"、"chat-done"、"chat-error"）
     * - `callback`: 事件回调函数，接收 { payload, id } 对象
     *
     * 返回:
     * - 取消监听的函数（调用后停止接收该事件）
     *
     * 示例:
     *   const unlisten = await TauriBridge.listen('chat-delta', (event) => {
     *       appendText(event.payload.content);
     *   });
     *   // 取消监听：
     *   unlisten();
     */
    async function listen(event, callback) {
        if (!_isTauri) {
            console.warn('[TauriBridge] 不在 Tauri 环境中，无法监听事件: ' + event);
            return function () { /* noop */ };
        }

        try {
            const unlisten = await window.__TAURI__.event.listen(event, function (eventData) {
                callback(eventData);
            });
            return unlisten;
        } catch (err) {
            console.error('[TauriBridge] 事件监听失败:', event, err);
            throw new Error('监听事件失败: ' + event + ' — ' + (err.message || String(err)));
        }
    }

    /**
     * 发射 Tauri 事件（前端 → 后端，或前端 → 前端）。
     *
     * 参数:
     * - `event`: 事件名称
     * - `payload`: 事件负载对象
     */
    async function emit(event, payload) {
        if (!_isTauri) {
            console.warn('[TauriBridge] 不在 Tauri 环境中，无法发射事件: ' + event);
            return;
        }

        try {
            await window.__TAURI__.event.emit(event, payload);
        } catch (err) {
            console.error('[TauriBridge] 事件发射失败:', event, err);
        }
    }

    // =========================================================
    // 公开 API
    // =========================================================

    return {
        invoke: invoke,
        listen: listen,
        emit: emit,
        /** 是否在 Tauri 环境中 */
        isTauri: function () { return _isTauri; },
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'TauriBridge', {
    value: TauriBridge,
    writable: false,
    configurable: false,
});
