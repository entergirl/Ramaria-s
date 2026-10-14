/**
 * tests/chat-readonly.test.js — 会话访问模式判定（node --test）
 *
 * 覆盖:
 * - 社交平台会话（qq / telegram）→ 'social' 只读（社交优先于已关闭）
 * - 已关闭本地会话 → 'closed' 只读
 * - 活跃本地 / mcp 会话 → ''（可正常输入）
 * - 边界：空通道、非时间戳值（0 / undefined）
 *
 * 运行: node --test "tests/*.test.js"
 *
 * 说明:
 * - chat.js 经 vm 沙箱加载：IIFE 顶层仅做函数定义，注入最小 Router 桩
 *   （registerHook 返回退注册函数、getCurrentView 返回非 chat，
 *   避免自动初始化分支触发视图重进定时器导致测试进程挂起）。
 */

'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

/** 对话视图源码（frontend/js/views/chat.js） */
const CHAT_PATH = path.resolve(__dirname, '..', 'js', 'views', 'chat.js');

/**
 * 在 vm 沙箱中加载 chat.js，返回 RamariaChatView 单例。
 *
 * 返回:
 * - 视图对象（至少含 resolveSessionAccess）。
 */
function loadChatView() {
  const source = fs.readFileSync(CHAT_PATH, 'utf8');
  const windowMock = {};
  const sandbox = {
    window: windowMock,
    // 静默输出：避免用例噪声；不阻断错误
    console: { log() {}, warn() {}, error() {}, debug() {} },
    document: {
      getElementById: () => null,
      querySelector: () => null,
      querySelectorAll: () => [],
    },
    setTimeout: () => 0,
    clearTimeout: () => {},
    RamariaRouter: {
      registerHook: () => () => {},
      getCurrentView: () => 'settings',
      setSessionInfo: () => {},
      setContentActions: () => {},
    },
    RamariaStore: {
      get: () => null,
      set: () => {},
      subscribe: () => () => {},
      getPersonaSession: () => null,
      setPersonaSession: () => {},
    },
  };
  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(source, sandbox, { filename: 'chat.js' });

  const view = windowMock.RamariaChatView;
  if (!view) throw new Error('chat.js 未暴露 RamariaChatView');
  return view;
}

const chatView = loadChatView();

test('访问模式：社交平台会话（qq / telegram）恒为 social 只读', () => {
  assert.equal(chatView.resolveSessionAccess('qq', null), 'social');
  assert.equal(chatView.resolveSessionAccess('telegram', null), 'social');
  assert.equal(chatView.resolveSessionAccess('qq', 1700000000000), 'social', '社交优先于已关闭');
});

test('访问模式：已关闭本地会话为 closed 只读', () => {
  assert.equal(chatView.resolveSessionAccess('local', 1700000000000), 'closed');
  assert.equal(chatView.resolveSessionAccess('mcp', 1), 'closed');
});

test('访问模式：活跃本地 / mcp 会话可正常输入', () => {
  assert.equal(chatView.resolveSessionAccess('local', null), '');
  assert.equal(chatView.resolveSessionAccess('mcp', 0), '', '0 视为未关闭');
  assert.equal(chatView.resolveSessionAccess('', null), '');
});

test('访问模式：边界输入不误判为只读', () => {
  assert.equal(chatView.resolveSessionAccess(undefined, null), '');
  assert.equal(chatView.resolveSessionAccess(null, null), '');
  assert.equal(chatView.resolveSessionAccess('QQ', null), '', '通道匹配为大小写敏感（存储口径小写）');
});
