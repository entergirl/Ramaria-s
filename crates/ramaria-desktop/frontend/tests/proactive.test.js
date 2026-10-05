/**
 * tests/proactive.test.js — 主动消息处理回归（node --test）
 *
 * 覆盖:
 * - init：注册 `proactive-message` 监听；重复 init 不重复注册；非 Tauri 跳过
 * - handlePayload：无效负载（非对象 / 缺 session_id / 缺 message_id）防御
 * - 常规消息：当前会话匹配时幂等追加（id / role / is_proactive / content 透传）
 * - 常规消息：非当前会话不追加，仅刷新会话列表
 * - 已读标记：当前会话匹配时调用 markRead；不匹配 / activated 路径不直接调用
 * - 幂等：messages 已含同 message_id 时不重复追加
 * - activated：当前视图为 chat → RamariaChatView.openSession；其它视图 → Router.showView
 *
 * 说明:
 * - proactive.js 是浏览器 IIFE；本测试用 vm 注入 window/console/TauriBridge/Store/
 *   Api/Router/ChatView 桩加载，只调用其公开 API，不触碰真实 DOM 与 Tauri；
 * - vm 沙箱返回的对象/数组原型属于沙箱 realm，断言前用 JSON 往返归一，
 *   避免 deepStrictEqual 的原型比较失败（与既有测试同一注意事项）。
 *
 * 运行: node --test "tests/*.test.js"
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

/** 主动消息模块源码（frontend/js/proactive.js） */
const PROACTIVE_PATH = path.resolve(__dirname, '..', 'js', 'proactive.js');

/** 等待一轮微任务/宏任务（listen Promise 的回调已 resolve） */
function tick() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

/** JSON 往返归一（vm realm 对象 → 宿主 realm） */
function plain(value) {
  return JSON.parse(JSON.stringify(value));
}

/**
 * 在 vm 沙箱中加载 proactive.js。
 *
 * 参数:
 * - `overrides`: 可选，预置状态 { activeSessionId, messages, sessions, currentView,
 *   isTauri, listResult, listenFails }。
 *
 * 返回:
 * - `proactive`: 全局单例 RamariaProactive；
 * - `state`: 调用记录（listenCalls / appended / storeSet / listCalls / actions / warnings）；
 * - `storeState`: 内存 Store 状态（断言最终值）。
 */
function loadProactive(overrides) {
  const opts = overrides || {};
  const source = fs.readFileSync(PROACTIVE_PATH, 'utf8');

  const storeState = {
    activeSessionId: opts.activeSessionId !== undefined ? opts.activeSessionId : null,
    messages: opts.messages || [],
    sessions: opts.sessions || [],
    currentView: opts.currentView !== undefined ? opts.currentView : 'chat',
  };
  const state = {
    listenCalls: [],
    appended: [],
    storeSet: [],
    listCalls: 0,
    markReadCalls: [],
    actions: [],
    warnings: [],
  };

  const sandbox = {
    window: {},
    console: {
      log() {},
      warn(msg) { state.warnings.push(String(msg)); },
      error() {},
    },
    TauriBridge: {
      isTauri() { return opts.isTauri !== undefined ? opts.isTauri : true; },
      listen(name, fn) {
        state.listenCalls.push({ name, fn });
        if (opts.listenFails) {
          return Promise.reject(new Error('listen-failed'));
        }
        return Promise.resolve(function () { /* unlisten 桩 */ });
      },
    },
    RamariaApi: {
      session: {
        list() {
          state.listCalls++;
          return Promise.resolve(opts.listResult || [{ id: 's1' }]);
        },
        markRead(id) {
          state.markReadCalls.push(id);
          return Promise.resolve('ok');
        },
      },
    },
    RamariaStore: {
      get(key) {
        return storeState[key];
      },
      set(key, value) {
        storeState[key] = value;
        state.storeSet.push({ key, value });
      },
      appendMessage(msg) {
        storeState.messages = storeState.messages.concat([msg]);
        state.appended.push(msg);
      },
    },
    RamariaRouter: {
      getCurrentView() { return storeState.currentView; },
      showView(view, options) {
        state.actions.push({ type: 'showView', view, options });
      },
    },
    RamariaChatView: {
      openSession(id) {
        state.actions.push({ type: 'openSession', id });
      },
    },
    RamariaToast: {
      show() {},
    },
  };
  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(source, sandbox, { filename: 'proactive.js' });

  const proactive = sandbox.window.RamariaProactive;
  if (!proactive) throw new Error('proactive.js 未暴露 RamariaProactive');
  return { proactive, state, storeState };
}

test('init: 注册 proactive-message 监听；重复 init 不重复注册', async () => {
  const { proactive, state } = loadProactive();

  proactive.init();
  await tick();

  assert.equal(state.listenCalls.length, 1, '应注册一次监听');
  assert.equal(state.listenCalls[0].name, 'proactive-message');
  assert.equal(typeof state.listenCalls[0].fn, 'function');

  // 重复调用：已有注册（或注册中）时跳过
  proactive.init();
  await tick();
  assert.equal(state.listenCalls.length, 1, '重复 init 不应再次注册');
});

test('init: 非 Tauri 环境跳过注册', async () => {
  const { proactive, state } = loadProactive({ isTauri: false });

  proactive.init();
  await tick();

  assert.equal(state.listenCalls.length, 0, '非 Tauri 环境不应注册监听');
  assert.ok(state.warnings.length > 0, '应输出降级告警');
});

test('handlePayload: 无效负载不追加消息且不抛异常', () => {
  const { proactive, state } = loadProactive({ activeSessionId: 's1' });

  assert.doesNotThrow(() => {
    proactive.handlePayload(null);
    proactive.handlePayload(undefined);
    proactive.handlePayload('not-an-object');
    proactive.handlePayload(42);
    proactive.handlePayload({});
    proactive.handlePayload({ session_id: 's1' });
    proactive.handlePayload({ message_id: 'm1' });
  });

  assert.equal(state.appended.length, 0, '无效负载不应追加消息');
  assert.equal(state.listCalls, 0, '无效负载不应刷新会话列表');
  assert.equal(state.actions.length, 0, '无效负载不应触发定位');
});

test('handlePayload: 常规消息且当前会话匹配 → 幂等追加 assistant 消息', async () => {
  const { proactive, state, storeState } = loadProactive({ activeSessionId: 's1' });

  proactive.handlePayload({
    session_id: 's1',
    message_id: 'm1',
    content: '在吗？突然想起你说过的事。',
    persona: 'rama-0001',
    source: 'event_follow_up',
    created_at: 1759459200000,
  });
  await tick();

  assert.equal(state.appended.length, 1);
  const msg = plain(state.appended[0]);
  assert.equal(msg.id, 'm1');
  assert.equal(msg.role, 'assistant');
  assert.equal(msg.content, '在吗？突然想起你说过的事。');
  assert.equal(msg.persona_uid, 'rama-0001');
  assert.equal(msg.created_at, 1759459200000);
  assert.equal(msg.is_proactive, true);

  // 常规消息不应触发会话定位
  assert.equal(state.actions.length, 0);
  assert.equal(storeState.messages.length, 1);
});

test('handlePayload: 常规消息且当前会话不匹配 → 不追加，仅刷新会话列表', async () => {
  const { proactive, state, storeState } = loadProactive({ activeSessionId: 's-other' });

  proactive.handlePayload({
    session_id: 's1',
    message_id: 'm1',
    content: '在吗',
    persona: 'rama-0001',
    source: 'event',
    created_at: 1759459200000,
  });
  await tick();

  assert.equal(state.appended.length, 0, '非当前会话不应追加消息');
  assert.equal(state.listCalls, 1, '应刷新会话列表');
  assert.deepEqual(plain(storeState.sessions), [{ id: 's1' }]);
  assert.equal(state.actions.length, 0);
});

test('handlePayload: 常规消息且当前会话匹配 → 标记该会话已读', async () => {
  const { proactive, state } = loadProactive({ activeSessionId: 's1' });

  proactive.handlePayload({
    session_id: 's1',
    message_id: 'm-unread-1',
    content: '在吗',
    persona: 'rama-0001',
    source: 'event',
    created_at: 1759459200000,
  });
  await tick();

  assert.deepEqual(state.markReadCalls, ['s1'], '当前查看的会话应标记已读');
});

test('handlePayload: 常规消息且当前会话不匹配 → 不标记已读', async () => {
  const { proactive, state } = loadProactive({ activeSessionId: 's-other' });

  proactive.handlePayload({
    session_id: 's1',
    message_id: 'm-unread-2',
    content: '在吗',
    persona: 'rama-0001',
    source: 'event',
    created_at: 1759459200000,
  });
  await tick();

  assert.deepEqual(state.markReadCalls, [], '非当前会话不应标记已读');
});

test('handlePayload: activated 重播路径不直接标记已读（由会话打开路径处理）', async () => {
  const { proactive, state } = loadProactive({ activeSessionId: 's1', currentView: 'chat' });

  proactive.handlePayload({
    session_id: 's1',
    message_id: 'm-unread-3',
    content: '在吗',
    persona: 'rama-0001',
    source: 'event',
    created_at: 1759459200000,
    activated: true,
  });
  await tick();

  assert.deepEqual(state.markReadCalls, [], 'activated 路径由 openSession 内部标记已读');
});

test('handlePayload: 同 message_id 已存在时不重复追加（幂等）', async () => {
  const { proactive, state } = loadProactive({
    activeSessionId: 's1',
    messages: [{ id: 'm1', role: 'assistant', content: '既有主动消息' }],
  });

  proactive.handlePayload({
    session_id: 's1',
    message_id: 'm1',
    content: '在吗',
    persona: 'rama-0001',
    source: 'event',
    created_at: 1759459200000,
  });
  await tick();

  assert.equal(state.appended.length, 0, '同 id 消息不应重复追加');
  assert.equal(state.actions.length, 0);
});

test('handlePayload: activated 且当前视图为 chat → ChatView.openSession 定位', async () => {
  const { proactive, state } = loadProactive({ activeSessionId: 's1', currentView: 'chat' });

  proactive.handlePayload({
    session_id: 's1',
    message_id: 'm2',
    content: '在吗',
    persona: 'rama-0001',
    source: 'event',
    created_at: 1759459200000,
    activated: true,
  });
  await tick();

  assert.deepEqual(plain(state.actions), [{ type: 'openSession', id: 's1' }]);
  assert.equal(state.listCalls, 0, 'activated 重播不应刷新会话列表');
  assert.equal(state.appended.length, 1, '当前会话匹配时仍应幂等追加');
});

test('handlePayload: activated 且当前视图非 chat → Router.showView 携带 sessionId', async () => {
  const { proactive, state } = loadProactive({ activeSessionId: null, currentView: 'settings' });

  proactive.handlePayload({
    session_id: 's1',
    message_id: 'm3',
    content: '在吗',
    persona: 'rama-0001',
    source: 'event',
    created_at: 1759459200000,
    activated: true,
  });
  await tick();

  assert.equal(state.actions.length, 1);
  const action = plain(state.actions[0]);
  assert.equal(action.type, 'showView');
  assert.equal(action.view, 'chat');
  assert.equal(action.options.sessionId, 's1');
  assert.equal(action.options.fromView, 'proactive');
  assert.equal(state.appended.length, 0, '非当前会话不应追加消息');
});
