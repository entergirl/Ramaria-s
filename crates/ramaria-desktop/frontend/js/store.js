/**
 * js/store.js — Ramaria 全局状态管理（发布订阅）
 *
 * 职责:
 * - 单例 Store，管理全局应用状态（appState / sessions / messages / config / personas）
 * - 通过 subscribe 提供发布订阅模式，状态变更时通知所有订阅者
 * - 不操作 DOM，不直接调用 TauriBridge，纯数据层
 * - 每个状态变更经过 set 方法，确保订阅者始终看到最新一致状态
 *
 * 设计特点:
 * - 在 Store 内部维护 _state 对象，保证引用一致性
 * - 公开 getter（只读）/ setter（触发通知）对，禁止外部直接修改 _state
 * - 事件名使用点分命名（'appState' / 'currentView' 等），与 Router 的事件监听对齐
 * - 支持一次性订阅（once），视图销毁时自动取消
 * - 订阅者回调执行错误被 try-catch 包裹，单个回调失败不影响其他订阅者
 *
 * 用法:
 * Store.subscribe('appState', function(newState, oldState) { ... });
 * Store.set('appState', 'Ready');
 * var state = Store.get('appState');
 *
 * 依赖: 无（独立模块，必须在 tauri-bridge.js 之后、其他模块之前加载）
 */

var RamariaStore = (function () {
    'use strict';

 // =========================================================
 // 内部状态（外部仅通过 get/set 访问）
 // =========================================================

 /**
 * 全局状态对象。
 *
 * 字段约定:
 * - `appState`: 应用状态，对齐 Rust AppState::as_str 返回值（snake_case）
 * "needs_setup" | "downloading_model" | "indexing" | "ready" | "degraded" | "fatal_error"
 * - `currentView`: 当前显示的视图名称
 * "setup" | "progress" | "chat" | "memory" | "settings" | "error"
 * - `sessions`: 会话数组 [{ id, started_at, ended_at, message_count }]
 * - `activeSessionId`: 当前活跃会话 UUID 字符串（null 表示无活跃会话）
 * - `messages`: 当前会话的消息数组 [{ id, role, content, persona_uid, created_at }]
 * - `isStreaming`: 是否正在流式接收 LLM 回复
 * - `streamingRequestId`: 当前流式请求的 request_id（null 表示无进行中流式）
 * - `backendConfig`: 后端配置 { provider, model_id, base_url, supports_streaming, ... }
 * - `settings`: 全局设置 [{ key, value }]
 * - `personas`: 已注册人格列表 [{ uid, name, kind, is_active, created_at }]
 * - `defaultPersonaUid`: 默认对话人格 UID（ 新增）
 */
    var _state = {
        appState: 'needs_setup',
        currentView: null,
        sessions: [],
        activeSessionId: null,
        messages: [],
        isStreaming: false,
        streamingRequestId: null,
        backendConfig: null,
        settings: [],
        personas: [],
        defaultPersonaUid: null,
        degradedReason: null,
 /// 导入完成页 → 记忆页导航预设 persona UID（一次性消费，读取后清除）
        preselectPersonaUid: null,
 /// 导入完成页 → 聊天页"查看导入消息"模式标志（一次性消费）
        viewingImportedSession: false,
 /// 导入消息的导出者 persona 名称（聊天页页眉展示用）
        viewingImportedName: '',
/// 人格→会话缓存 { persona_uid: session_id }
/// 降级为性能缓存——真相源在后端 sessions.persona_uid 字段。
/// 用于加速 persona 切换时的 session 查找，不依赖其作为归属判断依据。
/// 缓存可能在 persona 切换后过期（session.persona_uid 已更新但缓存未刷新），
/// 实际归属以 `sessionPersonaUid` 为准。
        personaSessions: {},
/// 当前选中的人格 UID（用于消息归属和页眉显示）
        currentPersonaUid: null,
/// 当前活跃 session 绑定的 persona_uid（真相源来自后端 session.persona_uid）。
/// 与 currentPersonaUid 的区别：前者来自前端下拉框选择，后者来自后端 DB 查询。
/// 正常情况下两者一致；不一致时（如后端已更新但前端尚未感知），
/// 以 sessionPersonaUid 为准做消息归属判断。
        sessionPersonaUid: null,
    };

 // =========================================================
 // 订阅者管理
 // =========================================================

 /**
 * 事件 → 回调集合映射。
 * 每个事件名下存储一组 { callback, once } 对象。
 */
    var _subscribers = {};

 /**
 * 订阅状态变更事件。
 *
 * 参数:
 * - `event`: 事件名称（如 'appState'、'currentView'、'messages' 等）
 * - `callback`: 回调函数，接收 (newValue, oldValue) 两个参数
 * - `once`: 可选，true 表示仅触发一次后自动取消订阅
 *
 * 返回:
 * - 取消订阅函数，调用后永久移除该回调
 */
    function subscribe(event, callback, once) {
        if (!_subscribers[event]) {
            _subscribers[event] = [];
        }

        var entry = { callback: callback, once: !!once };
        _subscribers[event].push(entry);

 // 返回取消订阅函数
        return function unsubscribe() {
            var list = _subscribers[event];
            if (!list) return;
            for (var i = list.length - 1; i >= 0; i--) {
                if (list[i] === entry) {
                    list.splice(i, 1);
                    break;
                }
            }
 // 如果该事件再无订阅者，清理空数组
            if (list.length === 0) {
                delete _subscribers[event];
            }
        };
    }

 /**
 * 一次性订阅，触发后自动取消。
 */
    function once(event, callback) {
        return subscribe(event, callback, true);
    }

 /**
 * 通知所有订阅者。
 *
 * 说明:
 * - 按注册顺序依次调用回调
 * - 每个回调被 try-catch 包裹，单个回调抛错不影响后续回调
 * - once 订阅者在回调执行后自动移除
 */
    function notify(event, newValue, oldValue) {
        var list = _subscribers[event];
        if (!list) return;

 // 复制列表，防止回调中修改订阅列表导致遍历问题
        var snapshot = list.slice();

        for (var i = 0; i < snapshot.length; i++) {
            var entry = snapshot[i];
            try {
                entry.callback(newValue, oldValue);
            } catch (err) {
                console.error('[Store] 订阅者回调异常 (事件: ' + event + '):', err);
            }

 // once 订阅者：回调执行后从原列表中移除
            if (entry.once) {
                var origList = _subscribers[event];
                if (origList) {
                    for (var j = origList.length - 1; j >= 0; j--) {
                        if (origList[j] === entry) {
                            origList.splice(j, 1);
                            break;
                        }
                    }
                }
            }
        }

 // 清理空事件列表
        if (_subscribers[event] && _subscribers[event].length === 0) {
            delete _subscribers[event];
        }
    }

 // =========================================================
 // 状态访问
 // =========================================================

 /**
 * 获取指定状态字段的当前值。
 *
 * 参数:
 * - `key`: 状态字段名
 *
 * 返回:
 * - 该字段的当前值（只读引用，请勿直接修改对象/数组内容）
 *
 * 说明:
 * - 对数组字段（sessions/messages/settings/personas），返回的是内部数组引用
 * 不建议直接 push/splice；应使用 set 方法整体替换以触发通知
 */
    function get(key) {
        if (!(key in _state)) {
            console.warn('[Store] 未知状态字段: ' + key);
            return undefined;
        }
        return _state[key];
    }

 /**
 * 设置指定状态字段的值，并通知订阅者。
 *
 * 参数:
 * - `key`: 状态字段名
 * - `value`: 新值
 * - `silent`: 可选，true 表示静默更新不触发通知（仅用于批量初始化）
 *
 * 说明:
 * - 值相同（===）时跳过通知，避免无意义渲染
 * - 对数组字段，建议传入新数组引用而非原地修改
 * - silent 模式用于批量加载数据时避免中间状态触发路由/UI 更新
 */
    function set(key, value, silent) {
        if (!(key in _state)) {
            console.warn('[Store] 未知状态字段: ' + key);
            return;
        }

        var oldValue = _state[key];

 // 值相同则跳过
        if (oldValue === value) {
            return;
        }

        _state[key] = value;

        if (!silent) {
            notify(key, value, oldValue);
        }

 // 调试日志（不含敏感数据）
        if (key === 'appState') {
            console.log('[Store] 状态变更: ' + key + ' = ' + value);
        } else if (key === 'isStreaming') {
            console.log('[Store] 流式状态: ' + (value ? '开始' : '结束'));
        }
    }

 /**
 * 批量静默设置多个状态字段。
 * 用于首屏数据加载，所有字段设置完成后统一触发 ready 事件。
 *
 * 参数:
 * - `map`: { key1: value1, key2: value2, ... }
 */
    function batchSet(map) {
        var keys = Object.keys(map);
        for (var i = 0; i < keys.length; i++) {
            var key = keys[i];
            if (key in _state) {
                _state[key] = map[key];
            }
        }
    }

 // =========================================================
 // 便捷方法
 // =========================================================

 /**
 * 向 messages 数组追加消息（不可变更新）。
 *
 * 参数:
 * - `message`: 消息对象 { id, role, content, persona_uid, created_at }
 */
    function appendMessage(message) {
        var msgs = _state.messages.slice(); // 浅拷贝
        msgs.push(message);
        set('messages', msgs);
    }

 /**
 * 更新 messages 数组最后一条消息（用于流式追加文本）。
 *
 * 参数:
 * - `updater`: 接收最后一条消息的引用副本，返回更新后的消息对象
 * 如果 messages 为空则不做任何操作
 */
    function updateLastMessage(updater) {
        var msgs = _state.messages;
        if (msgs.length === 0) return;

        var updated = msgs.slice();
        var last = Object.assign({}, updated[updated.length - 1]);
        var newLast = updater(last);
        if (newLast) {
            updated[updated.length - 1] = newLast;
            set('messages', updated);
        }
    }

 /**
 * 获取当前订阅者数量（调试用）。
 *
 * 返回:
 * - 每个事件的订阅数量对象
 */
    function subscriberCount() {
        var counts = {};
        var events = Object.keys(_subscribers);
        for (var i = 0; i < events.length; i++) {
            counts[events[i]] = _subscribers[events[i]].length;
        }
        return counts;
    }

 /**
 * 清除所有订阅者（用于应用重置/视图销毁）。
 */
    function clearSubscribers() {
        _subscribers = {};
    }

 /**
 * 重置状态到初始值（用于退出/重置应用）。
 */
    function reset() {
        _state = {
            appState: 'needs_setup',
            currentView: null,
            sessions: [],
            activeSessionId: null,
            messages: [],
            isStreaming: false,
            streamingRequestId: null,
            backendConfig: null,
            settings: [],
            personas: [],
            defaultPersonaUid: null,
            degradedReason: null,
            personaSessions: {},
            currentPersonaUid: null,
            sessionPersonaUid: null,
            preselectPersonaUid: null,
            viewingImportedSession: false,
            viewingImportedName: '',
        };
        _subscribers = {};
        console.log('[Store] 状态已重置');
    }

 // =========================================================
 // 公开 API
 // =========================================================

 // =========================================================
 // 人格会话映射
 // =========================================================

 /**
 * 获取指定人格的活跃 session ID。
 *
 * 参数:
 * - `personaUid`: 人格业务标识。
 *
 * 返回:
 * - session_id 或 null。
 */
    function getPersonaSession(personaUid) {
        if (!personaUid) return null;
        return _state.personaSessions[personaUid] || null;
    }

 /**
 * 设置指定人格的活跃 session ID（立即写入 Store + 通知订阅者）。
 *
 * 参数:
 * - `personaUid`: 人格业务标识。
 * - `sessionId`: 会话 UUID。
 */
    function setPersonaSession(personaUid, sessionId) {
        if (!personaUid) return;
        var map = Object.assign({}, _state.personaSessions);
        map[personaUid] = sessionId;
        set('personaSessions', map);
    }

    return {
        get: get,
        set: set,
        batchSet: batchSet,
        subscribe: subscribe,
        once: once,
        appendMessage: appendMessage,
        updateLastMessage: updateLastMessage,
        subscriberCount: subscriberCount,
        clearSubscribers: clearSubscribers,
        reset: reset,
 /** 获取完整状态快照（只读，调试用） */
        snapshot: function () { return Object.assign({}, _state); },
 /** 人格会话映射 */
        getPersonaSession: getPersonaSession,
        setPersonaSession: setPersonaSession,
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaStore', {
    value: RamariaStore,
    writable: false,
    configurable: false,
});
